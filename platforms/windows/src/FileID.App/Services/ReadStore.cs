// ReadStore — app-side read-only SQLite access.
// `Open()` is async so the call site can await schema validation without blocking the UI thread.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Data.Sqlite;

namespace FileID.Services;

internal sealed class ReadStore : IAsyncDisposable, IDisposable, INotifyPropertyChanged
{
    private readonly string _dbPath;
    private readonly string _connString;
    private SqliteConnection? _connection;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private int _disposed;

    public event PropertyChangedEventHandler? PropertyChanged;

    /// <summary>Humanized last error from OpenAsync — a DB-open timeout, permission denial, lock, or corruption.</summary>
    private string? _lastOpenError;
    public string? LastOpenError
    {
        get => _lastOpenError;
        private set
        {
            if (_lastOpenError == value) return;
            _lastOpenError = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LastOpenError)));
        }
    }

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

    private volatile ViewModels.LibraryScope? _scope;

    public ViewModels.LibraryScope? Scope
    {
        get => _scope;
        set => _scope = value;
    }

    private static string ScopeClause(string column, ViewModels.LibraryScope? scope) =>
        scope is null ? "" : " AND " + ViewModels.LibraryScope.Sql(column, "$scope");

    private static void BindScope(SqliteCommand cmd, ViewModels.LibraryScope? scope)
    {
        if (scope is not null) cmd.Parameters.AddWithValue("$scope", scope.Param);
    }

    public async Task OpenAsync(CancellationToken ct = default)
    {
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection != null)
            {
                return;
            }
            // On network shares or slow USB sticks this sync call can block the UI for hundreds of ms; the cost on a local SSD is < 1 ms so there's no downside to always going async here.
            var dbPath = _dbPath;
            // We'd rather treat it as "DB not present" after a few seconds than freeze startup.
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
                    // A 5s stall on the DB path is an actionable degradation (disconnected SMB share / unplugged USB), not the silent first-launch "DB not yet created" case — surface it so the caller shows a dismissible "drive unreachable" message rather than an indistinguishable empty grid.
                    DebugLog.Warn($"ReadStore.OpenAsync: File.Exists timed out for {PathRedactor.Redact(dbPath)}; treating as missing.");
                    LastOpenError = "FileID couldn't reach the library folder (the drive may be disconnected or slow). Check the drive is connected and try again.";
                    return;
                }
            }
            if (!dbExists)
            {
                return;
            }
            SqliteConnection? conn = null;
            try
            {
                conn = new SqliteConnection(_connString);
                await conn.OpenAsync(ct).ConfigureAwait(false);
                // Match the engine's PRAGMA shape so reads see the same cache / mmap behavior as the writer (these are no-ops on read-only, but documented for parity).
                using (var cmd = conn.CreateCommand())
                {
                    cmd.CommandText = "PRAGMA query_only = ON;";
                    await cmd.ExecuteNonQueryAsync(ct).ConfigureAwait(false);
                }
                _connection = conn;
                conn = null;
                LastOpenError = null;
            }
            catch (OperationCanceledException)
            {
                conn?.Dispose();
                throw; // caller-initiated cancellation — not an error to surface
            }
            catch (Exception ex)
            {
                // DB open / first-PRAGMA failure: locked (BUSY), permission denied (CANTOPEN), corrupt image, full disk, IO error.
                conn?.Dispose();
                LastOpenError = SqliteErrorTranslator.Humanize(ex);
                DebugLog.Warn($"ReadStore.OpenAsync: open failed for {PathRedactor.Redact(_dbPath)}: {ex.Message}");
                throw;
            }
        }
        finally
        {
            _gate.Release();
        }
    }

    public async Task<IReadOnlyList<FileRow>> SearchAsync(
        string query, int limit, CancellationToken ct, string? kind = null)
    {
        if (_connection == null)
        {
            return Array.Empty<FileRow>();
        }
        var trimmedSearch = query?.Trim();
        if (string.IsNullOrEmpty(trimmedSearch))
        {
            return await RecentAsync(limit, ct, kind).ConfigureAwait(false);
        }
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();
            var rows = new List<FileRow>(limit);
            var seen = new HashSet<long>();
            using var cmd = _connection.CreateCommand();

            // NFC-normalize first (macOS parity): SQLite LIKE compares bytes, and path_search stores the NFC form (v16) so an NFC query matches Mac/NAS-synced NFD names too.
            var escapedSearch = trimmedSearch
                .Normalize(NormalizationForm.FormC)
                .Replace("\\", "\\\\")
                .Replace("%", "\\%")
                .Replace("_", "\\_");
            var like = $"%{escapedSearch}%";
            var match = BuildMatchExpression(trimmedSearch);
            var hasMatch = !string.IsNullOrEmpty(match);
            var kindClause = string.IsNullOrEmpty(kind) || kind == "all" ? "" : " AND f.kind = $kind";
            var scope = _scope;
            kindClause += ScopeClause("f.path_text", scope);
            BindScope(cmd, scope);

            cmd.CommandText = $"""
            SELECT f.id, f.path_text, f.kind, f.size_bytes, f.modified_at, f.has_faces, f.has_text,
                   (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = f.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                   f.vlm_proposed_name
            FROM files f
            WHERE f.failed = 0{kindClause}
              AND (
                   {(hasMatch ? "f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH $match) OR f.id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH $match)" : "0")}
                   OR f.path_search LIKE $like ESCAPE '\'
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
            ORDER BY f.scanned_at DESC, f.id DESC
            LIMIT $limit
            """;

            if (hasMatch)
            {
                cmd.Parameters.AddWithValue("$match", match);
            }
            cmd.Parameters.AddWithValue("$like", like);
            cmd.Parameters.AddWithValue("$limit", limit);
            if (!string.IsNullOrEmpty(kind) && kind != "all") cmd.Parameters.AddWithValue("$kind", kind);

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

    public async Task<IReadOnlyList<FileRow>> RecentAsync(int limit, CancellationToken ct, string? kind = null, int offset = 0)
    {
        if (_connection == null) return Array.Empty<FileRow>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();
            var rows = new List<FileRow>(limit);
            using var cmd = _connection.CreateCommand();
            // `failed = 0` is the BASE predicate (matches SearchAsync / SemanticSearchAsync and the macOS reference) so files the engine flagged failed (corrupt / unreadable / permission-denied) don't leak into the default grid as broken placeholder tiles and inflate the count-vs-grid mismatch.
            var kindClause = string.IsNullOrEmpty(kind) || kind == "all" ? "" : " AND kind = $kind";
            var scope = _scope;
            var scopeClause = ScopeClause("path_text", scope);
            BindScope(cmd, scope);
            cmd.CommandText = $"""
                SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text,
                       (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = files.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                       vlm_proposed_name
                FROM files WHERE failed = 0{kindClause}{scopeClause}
                ORDER BY scanned_at DESC, id DESC LIMIT $limit OFFSET $offset
                """;
            cmd.Parameters.AddWithValue("$limit", limit);
            cmd.Parameters.AddWithValue("$offset", Math.Max(0, offset));
            if (kindClause.Length > 0) cmd.Parameters.AddWithValue("$kind", kind!);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                rows.Add(ReadRow(reader));
            }
            return rows;
        }
        finally { _gate.Release(); }
    }

    public async Task<IReadOnlyList<FileRowWithScore>> SemanticSearchAsync(
        float[] queryEmbedding, int limit, CancellationToken ct, string? kind = null)
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
            // exclude failed files — a corrupt file with a stale CLIP embedding must not surface in semantic results.
            var kindClause = string.IsNullOrEmpty(kind) || kind == "all" ? "" : " AND f.kind = $kind";
            var scope = _scope;
            var scopeClause = ScopeClause("f.path_text", scope);
            BindScope(cmd, scope);
            cmd.CommandText = $"""
            SELECT f.id, f.path_text, f.kind, f.size_bytes, f.modified_at,
                   f.has_faces, f.has_text,
                   (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = f.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                   f.vlm_proposed_name,
                   e.embedding
            FROM clip_embeddings e
            JOIN files f ON f.id = e.file_id
            WHERE f.failed = 0{kindClause}{scopeClause}
            """;
            if (kindClause.Length > 0) cmd.Parameters.AddWithValue("$kind", kind!);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                var blob = (byte[])reader.GetValue(9);
                if (blob.Length != queryEmbedding.Length * 4) continue;
                float score = DotProduct(queryEmbedding, blob);
                // Reading by column index stays valid until the next ReadAsync, so a lazy materialize inside the enqueue branches is byte-identical.
                if (heap.Count < limit)
                {
                    heap.Enqueue(new FileRowWithScore(ReadRow(reader), score), score);
                }
                else if (heap.TryPeek(out _, out var minScore) && score > minScore)
                {
                    heap.Dequeue();
                    heap.Enqueue(new FileRowWithScore(ReadRow(reader), score), score);
                }
            }
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

            var heap = new PriorityQueue<long, float>(limit);
            using (var cmd = _connection.CreateCommand())
            {
                var scope = _scope;
                cmd.CommandText = scope is null
                    ? "SELECT file_id, embedding FROM clip_embeddings WHERE file_id != $seedId"
                    : "SELECT e.file_id, e.embedding FROM clip_embeddings e JOIN files f ON f.id = e.file_id WHERE e.file_id != $seedId" + ScopeClause("f.path_text", scope);
                cmd.Parameters.AddWithValue("$seedId", seedId);
                BindScope(cmd, scope);
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                while (await reader.ReadAsync(ct).ConfigureAwait(false))
                {
                    var fid = reader.GetInt64(0);
                    var blob = (byte[])reader.GetValue(1);
                    if (blob.Length != seedVec.Length * 4) continue;
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

            return await RowsInOrderLockedAsync(_connection, topIDs, ct).ConfigureAwait(false);
        }
        finally { _gate.Release(); }
    }

    /// <summary>Library rows for ids, in that order.</summary>
    public async Task<IReadOnlyList<FileRow>> RowsByIdsAsync(IReadOnlyList<long> ids, CancellationToken ct)
    {
        if (_connection == null || ids.Count == 0) return Array.Empty<FileRow>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();
            return await RowsInOrderLockedAsync(_connection, ids, ct).ConfigureAwait(false);
        }
        finally { _gate.Release(); }
    }

    private async Task<IReadOnlyList<FileRow>> RowsInOrderLockedAsync(
        SqliteConnection connection, IReadOnlyList<long> ids, CancellationToken ct)
    {
        var rows = new List<FileRow>(ids.Count);
        var idPlaceholders = new List<string>(ids.Count);
        using var cmd = connection.CreateCommand();
        for (int j = 0; j < ids.Count; j++)
        {
            var paramName = $"$id{j}";
            idPlaceholders.Add(paramName);
            cmd.Parameters.AddWithValue(paramName, ids[j]);
        }
        var scope = _scope;
        cmd.CommandText = $"""
            SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text,
                   (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = files.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                   vlm_proposed_name
            FROM files
            WHERE id IN ({string.Join(",", idPlaceholders)}) AND failed = 0{ScopeClause("path_text", scope)}
            """;
        BindScope(cmd, scope);

        var byId = new Dictionary<long, FileRow>(ids.Count);
        using (var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false))
        {
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                var row = ReadRow(reader);
                byId[row.Id] = row;
            }
        }

        foreach (var id in ids)
        {
            if (byId.Remove(id, out var row))
            {
                rows.Add(row);
            }
        }
        return rows;
    }

    /// <summary>Powers the Deep Analyze "Pending renames (N)" pill so the user can bulk-apply the VLM's suggestions without manually walking the library.</summary>
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
                WHERE failed = 0
                  AND vlm_proposed_name IS NOT NULL
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

    public async Task<IReadOnlyDictionary<string, List<long>>> KeywordTagFileIdsAsync(CancellationToken ct)
    {
        var map = new Dictionary<string, List<long>>(StringComparer.Ordinal);
        if (_connection == null) return map;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return map;
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = """
                SELECT tags.file_id, tags.tag
                FROM tags
                INNER JOIN files ON files.id = tags.file_id
                WHERE files.failed = 0 AND tags.tag IS NOT NULL AND tags.tag != ''
                """;
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                long fileId = reader.GetInt64(0);
                string tag = reader.GetString(1);
                if (!map.TryGetValue(tag, out var ids)) { ids = new List<long>(); map[tag] = ids; }
                ids.Add(fileId);
            }
            return map;
        }
        finally { _gate.Release(); }
    }

    public async Task<IReadOnlyDictionary<string, List<long>>> NamedPersonFileIdsAsync(CancellationToken ct)
    {
        var map = new Dictionary<string, List<long>>(StringComparer.Ordinal);
        if (_connection == null) return map;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return map;
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = """
                SELECT face_prints.file_id, persons.title, persons.first_name,
                       persons.middle_name, persons.last_name, persons.suffix, persons.name
                FROM persons
                INNER JOIN face_prints ON face_prints.person_id = persons.id
                INNER JOIN files ON files.id = face_prints.file_id
                WHERE IFNULL(persons.is_unknown, 0) = 0
                  AND (persons.name IS NOT NULL OR persons.first_name IS NOT NULL
                       OR persons.title IS NOT NULL OR persons.last_name IS NOT NULL)
                  AND files.failed = 0
                """;
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                long fileId = reader.GetInt64(0);
                string name = FormatPersonTagName(
                    reader.IsDBNull(1) ? null : reader.GetString(1),
                    reader.IsDBNull(2) ? null : reader.GetString(2),
                    reader.IsDBNull(3) ? null : reader.GetString(3),
                    reader.IsDBNull(4) ? null : reader.GetString(4),
                    reader.IsDBNull(5) ? null : reader.GetString(5),
                    reader.IsDBNull(6) ? null : reader.GetString(6));
                if (name.Length == 0) continue;
                if (!map.TryGetValue(name, out var ids)) { ids = new List<long>(); map[name] = ids; }
                if (!ids.Contains(fileId)) ids.Add(fileId);
            }
            return map;
        }
        finally { _gate.Release(); }
    }

    public async Task<IReadOnlyList<long>> PersonFileIdsAsync(long personId, CancellationToken ct)
    {
        var ids = new List<long>();
        if (_connection == null) return ids;

        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return ids;

            using var cmd = _connection.CreateCommand();
            cmd.CommandText = """
                SELECT DISTINCT fp.file_id
                FROM face_prints fp
                INNER JOIN files f ON f.id = fp.file_id
                WHERE fp.person_id = $personId AND f.failed = 0
                ORDER BY fp.file_id
                """;
            cmd.Parameters.AddWithValue("$personId", personId);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                ids.Add(reader.GetInt64(0));
            }

            return ids;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>Byte-faithful with the macOS `ReadStore.personTagName` so a person is tagged identically on both platforms.</summary>
    internal static string FormatPersonTagName(string? title, string? first, string? middle,
                                              string? last, string? suffix, string? legacy)
    {
        var parts = new List<string>(5);
        foreach (var s in new[] { title, first, middle, last, suffix })
        {
            var t = (s ?? "").Trim();
            if (t.Length > 0) parts.Add(t);
        }
        if (parts.Count > 0) return string.Join(" ", parts);
        return (legacy ?? "").Trim();
    }

    public async Task<int> PendingProposedRenameCountAsync(CancellationToken ct)
    {
        if (_connection == null) return 0;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return 0;
            using var cmd = _connection.CreateCommand();
            cmd.CommandText =
                "SELECT COUNT(*) FROM files WHERE failed = 0 AND vlm_proposed_name IS NOT NULL AND vlm_proposed_name != ''";
            var result = await cmd.ExecuteScalarAsync(ct).ConfigureAwait(false);
            return result is null ? 0 : Convert.ToInt32(result);
        }
        finally { _gate.Release(); }
    }

    public async Task<FileDetails?> FileDetailsAsync(long fileId, CancellationToken ct)
    {
        if (_connection == null) return null;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return null;
            string? camera = null;
            double? captured = null;
            string? caption = null;
            var captionInherited = false;
            using (var cmd = _connection.CreateCommand())
            {
                cmd.CommandText = """
                    SELECT camera_model, COALESCE(created_at, modified_at), vlm_description, vlm_inherited_from IS NOT NULL
                    FROM files WHERE id = $id
                    """;
                cmd.Parameters.AddWithValue("$id", fileId);
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                if (!await reader.ReadAsync(ct).ConfigureAwait(false)) return null;
                camera = reader.IsDBNull(0) ? null : reader.GetString(0);
                captured = reader.IsDBNull(1) ? null : reader.GetDouble(1);
                caption = reader.IsDBNull(2) ? null : reader.GetString(2);
                captionInherited = !reader.IsDBNull(3) && reader.GetInt64(3) != 0;
            }

            var people = new List<string>();
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            using (var cmd = _connection.CreateCommand())
            {
                cmd.CommandText = """
                    SELECT DISTINCT p.id, p.title, p.first_name, p.middle_name, p.last_name, p.suffix, p.name
                    FROM face_prints f JOIN persons p ON p.id = f.person_id
                    WHERE f.file_id = $id AND IFNULL(p.is_unknown, 0) = 0
                    ORDER BY p.id
                    """;
                cmd.Parameters.AddWithValue("$id", fileId);
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                while (await reader.ReadAsync(ct).ConfigureAwait(false))
                {
                    string? Col(int i) => reader.IsDBNull(i) ? null : reader.GetString(i);
                    var name = FormatPersonTagName(Col(1), Col(2), Col(3), Col(4), Col(5), Col(6));
                    if (!string.IsNullOrEmpty(name) && seen.Add(name)) people.Add(name);
                }
            }
            return new FileDetails(camera, captured, caption, people, captionInherited);
        }
        finally { _gate.Release(); }
    }

    public async Task<IReadOnlyDictionary<long, ChatFileRow>> FilesByIdAsync(IReadOnlyCollection<long> ids, CancellationToken ct)
    {
        var rows = new Dictionary<long, ChatFileRow>();
        if (_connection == null || ids.Count == 0) return rows;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return rows;
            foreach (var chunk in ids.Distinct().Chunk(500))
            {
                using var cmd = _connection.CreateCommand();
                var names = new string[chunk.Length];
                for (var i = 0; i < chunk.Length; i++)
                {
                    names[i] = "$id" + i;
                    cmd.Parameters.AddWithValue(names[i], chunk[i]);
                }
                cmd.CommandText = $"SELECT id, path_text, kind, size_bytes, vlm_proposed_name FROM files WHERE failed = 0 AND id IN ({string.Join(',', names)})";
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                while (await reader.ReadAsync(ct).ConfigureAwait(false))
                {
                    var id = reader.GetInt64(0);
                    rows[id] = new ChatFileRow(id, reader.GetString(1), reader.GetString(2), reader.IsDBNull(3) ? 0 : reader.GetInt64(3), reader.IsDBNull(4) ? null : reader.GetString(4));
                }
            }
            return rows;
        }
        finally { _gate.Release(); }
    }

    public async Task<PersonNameRow?> PersonNameAsync(long personId, CancellationToken ct)
    {
        if (_connection == null) return null;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return null;
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = "SELECT title, first_name, middle_name, last_name, suffix, name, IFNULL(is_unknown, 0) FROM persons WHERE id = $id";
            cmd.Parameters.AddWithValue("$id", personId);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            if (!await reader.ReadAsync(ct).ConfigureAwait(false)) return null;
            string? Col(int i) => reader.IsDBNull(i) ? null : reader.GetString(i);
            return new PersonNameRow(Col(0), Col(1), Col(2), Col(3), Col(4), Col(5), reader.GetInt64(6) != 0);
        }
        finally { _gate.Release(); }
    }

    internal const int TimelineSpanLimit = 5000;

    public async Task<TimelineTracks> TimelineTracksAsync(long fileId, CancellationToken ct)
    {
        if (_connection == null) return TimelineTracks.Empty;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return TimelineTracks.Empty;
            var examined = await TimelineSpansAsync(_connection, """
                SELECT start_seconds, end_seconds FROM catalog_coverage
                WHERE file_id = $id AND status IN ('sampled', 'verified') AND id NOT LIKE 'speech:%'
                ORDER BY start_seconds LIMIT $limit
                """, fileId, ct).ConfigureAwait(false);
            var speech = await TimelineSpansAsync(_connection, """
                SELECT start_seconds, COALESCE(end_seconds, start_seconds) FROM catalog_passages
                WHERE file_id = $id AND stale = 0 AND id LIKE 'speech:%' AND start_seconds IS NOT NULL
                ORDER BY start_seconds LIMIT $limit
                """, fileId, ct).ConfigureAwait(false);
            return new TimelineTracks(examined, speech);
        }
        finally { _gate.Release(); }
    }

    private static async Task<IReadOnlyList<(double Start, double End)>> TimelineSpansAsync(
        SqliteConnection connection, string sql, long fileId, CancellationToken ct)
    {
        var spans = new List<(double Start, double End)>();
        try
        {
            using var cmd = connection.CreateCommand();
            cmd.CommandText = sql;
            cmd.Parameters.AddWithValue("$id", fileId);
            cmd.Parameters.AddWithValue("$limit", TimelineSpanLimit);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                if (reader.IsDBNull(0)) continue;
                var start = reader.GetDouble(0);
                spans.Add((start, reader.IsDBNull(1) ? start : reader.GetDouble(1)));
            }
        }
        catch (SqliteException ex)
        {
            DebugLog.Warn("ReadStore.TimelineTracksAsync: lane unavailable: " + ex.Message);
            spans.Clear();
        }
        return spans;
    }

    public async Task<IReadOnlyDictionary<string, int>> KindCountsAsync(CancellationToken ct)
    {
        if (_connection == null) return new Dictionary<string, int>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return new Dictionary<string, int>();
            var dict = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
            using var cmd = _connection.CreateCommand();
            var scope = _scope;
            cmd.CommandText = "SELECT kind, COUNT(*) FROM files WHERE failed = 0" + ScopeClause("path_text", scope) + " GROUP BY kind";
            BindScope(cmd, scope);
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
        System.Collections.Generic.IReadOnlyList<string>? tags = null;
        if (reader.FieldCount > 7 && !reader.IsDBNull(7))
        {
            var raw = reader.GetString(7);
            if (!string.IsNullOrEmpty(raw))
            {
                var parts = raw.Split('|', StringSplitOptions.RemoveEmptyEntries);
                var kept = new System.Collections.Generic.List<string>(parts.Length);
                foreach (var p in parts)
                {
                    var lower = p.Trim().ToLowerInvariant();
                    if (lower is "image" or "photo" or "photograph" or "photography" or "picture" or "face")
                    {
                        continue;
                    }
                    kept.Add(p);
                }
                tags = kept;
            }
        }
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
        // Drop tokens shorter than 2 chars to avoid degenerate explosion of matches.
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

    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0) return;
        // Reads ConfigureAwait(false) throughout and never re-enter the gate, so this can't self-deadlock.
        await _gate.WaitAsync().ConfigureAwait(false);
        try
        {
            _connection?.Dispose();
            _connection = null;
        }
        finally
        {
            _gate.Release();
        }
        _gate.Dispose();
    }

    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0) return;
        _gate.Wait();
        try
        {
            _connection?.Dispose();
            _connection = null;
        }
        finally
        {
            _gate.Release();
        }
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

internal sealed record ChatFileRow(long Id, string Path, string Kind, long SizeBytes, string? ProposedName);

internal sealed record PersonNameRow(string? Title, string? First, string? Middle, string? Last, string? Suffix, string? Name, bool IsUnknown);

internal sealed record TimelineTracks(
    System.Collections.Generic.IReadOnlyList<(double Start, double End)> Examined,
    System.Collections.Generic.IReadOnlyList<(double Start, double End)> Speech)
{
    public static TimelineTracks Empty { get; } = new(Array.Empty<(double, double)>(), Array.Empty<(double, double)>());
}

internal sealed record FileDetails(
    string? CameraModel,
    double? CapturedAt,
    string? Caption,
    System.Collections.Generic.IReadOnlyList<string> People,
    bool CaptionInherited = false);

internal sealed record ProposedRenameRow(
    long Id,
    string Path,
    string ProposedName);
