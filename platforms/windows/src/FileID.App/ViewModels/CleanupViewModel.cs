// CleanupViewModel — backs the Cleanup tab duplicate groups list.
//
// Groups files by exact `content_hash` (BLAKE3 for files <=16 MB, else a
// head+tail+size composite; migration v8) so each group is byte-for-byte
// identical, not merely visually similar. An identical size_bytes is required
// too as a cheap guard. Each group lets the user mark one keeper and trash the
// others (engine `trashFiles` IPC command, parallel IFileOperation::DeleteItem
// with FOF_ALLOWUNDO).

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;
using FileID.Services;
using Microsoft.Data.Sqlite;
using Microsoft.UI.Dispatching;

namespace FileID.ViewModels;

internal sealed class CleanupViewModel : INotifyPropertyChanged, IDisposable
{
    private readonly string _dbPath;
    private readonly DispatcherQueue _ui;
    private bool _isLoading;
    private string? _errorMessage;
    private bool _disposed;
    /// <summary>Cancelled in <see cref="Dispose"/> so a Refresh running on a
    /// thread-pool thread unwinds before the view is gone.</summary>
    private readonly CancellationTokenSource _disposalCts = new();

    public CleanupViewModel(string dbPath, DispatcherQueue ui)
    {
        _dbPath = dbPath;
        _ui = ui;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _disposalCts.Cancel(); } catch { /* swallow */ }
        try { _disposalCts.Dispose(); } catch { /* swallow */ }
    }

    public ObservableCollection<DuplicateGroup> Groups { get; } = new();

    public bool IsLoading
    {
        get => _isLoading;
        private set { if (_isLoading != value) { _isLoading = value; OnPropertyChanged(); } }
    }

    public string? ErrorMessage
    {
        get => _errorMessage;
        private set { if (_errorMessage != value) { _errorMessage = value; OnPropertyChanged(); } }
    }

    public async Task RefreshAsync(CancellationToken ct)
    {
        if (_disposed) return;
        try
        {
            // Linked token created inside the try: a Dispose() race after the
            // _disposed check makes _disposalCts.Token throw ObjectDisposedException,
            // caught below as a clean teardown no-op instead of escaping to the caller.
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            IsLoading = true;
            ErrorMessage = null;
            var groups = await Task.Run(() => Load(token), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            ApplyOnUi(groups);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        // Surface DB/IO failures as an actionable message instead of the raw
        // SQLite jargon ("database disk image is malformed") the user can't act on.
        catch (SqliteException ex) { if (!_disposed) ErrorMessage = SqliteErrorTranslator.Humanize(ex); }
        catch (IOException ex) { if (!_disposed) ErrorMessage = SqliteErrorTranslator.Humanize(ex); }
        catch (Exception ex) { if (!_disposed) ErrorMessage = ex.Message; }
        finally { if (!_disposed) IsLoading = false; }
    }

    /// <summary>Files larger than this use a head+tail+size COMPOSITE
    /// content_hash in the engine, not a full BLAKE3 — so matching hashes are
    /// "likely", not byte-verified. Mirror of the engine's FULL_HASH_MAX_BYTES.</summary>
    private const long FullHashMaxBytes = 16L * 1024 * 1024;

    private List<DuplicateGroup> Load(CancellationToken ct)
    {
        // First-launch guard: the engine creates the DB on first scan.
        if (!File.Exists(_dbPath))
        {
            return new List<DuplicateGroup>();
        }
        var connString = new SqliteConnectionStringBuilder
        {
            DataSource = _dbPath,
            Mode = SqliteOpenMode.ReadOnly,
        }.ToString();
        using var conn = new SqliteConnection(connString);
        conn.Open();
        // Pull every file with a content hash and group by EXACT equality —
        // identical content_hash AND size_bytes is byte-for-byte identical (1:1
        // duplicates), not just visually similar. content_hash is a BLOB
        // (BLAKE3 / composite, migration v8); read it as bytes and hex-encode
        // for a stable dictionary key. Grouping is O(n) via a dictionary, so
        // there's no per-pair scan and no candidate cap.
        using var cmd = conn.CreateCommand();
        cmd.CommandText = """
            SELECT id, path_text, size_bytes, content_hash
            FROM files
            WHERE content_hash IS NOT NULL AND failed = 0
            """;
        var rawMembers = new List<(long Id, string Path, long Size, string Hash)>(2048);
        using (var reader = cmd.ExecuteReader())
        {
            while (reader.Read())
            {
                ct.ThrowIfCancellationRequested();
                var hashBytes = (byte[])reader[3];
                if (hashBytes is null || hashBytes.Length == 0) continue;
                var hashHex = Convert.ToHexString(hashBytes);
                rawMembers.Add((reader.GetInt64(0), reader.GetString(1), reader.GetInt64(2), hashHex));
            }
        }

        // Group by composite key (content_hash + size): identical content AND
        // size means byte-for-byte identical. O(n) via a dictionary.
        var byHash = new Dictionary<string, List<int>>(rawMembers.Count);
        for (int i = 0; i < rawMembers.Count; i++)
        {
            ct.ThrowIfCancellationRequested();
            var key = rawMembers[i].Hash + ":" + rawMembers[i].Size.ToString();
            if (!byHash.TryGetValue(key, out var list)) { list = new List<int>(); byHash[key] = list; }
            list.Add(i);
        }

        var groups = new List<DuplicateGroup>();
        foreach (var (_, indices) in byHash)
        {
            if (indices.Count < 2) continue;
            // All members share identical bytes (and size); order by path for a
            // stable display and keep the first as the default keeper. The user
            // can re-pick in the UI.
            indices.Sort((a, b) => string.CompareOrdinal(rawMembers[a].Path, rawMembers[b].Path));
            var hash = rawMembers[indices[0]].Hash;
            // shared GroupName for the keeper RadioButton so mutual exclusion
            // within a duplicate group works. The content hash uniquely
            // identifies the group across the whole tab.
            var groupKey = $"dup-{hash}";
            var members = new List<DuplicateMember>(indices.Count);
            for (int k = 0; k < indices.Count; k++)
            {
                var m = rawMembers[indices[k]];
                members.Add(new DuplicateMember
                {
                    Id = m.Id,
                    Path = m.Path,
                    FileName = System.IO.Path.GetFileName(m.Path),
                    SizeBytes = m.Size,
                    GroupKey = groupKey,
                    IsKeeper = k == 0,
                });
            }
            groups.Add(new DuplicateGroup
            {
                ContentHash = hash,
                Members = members,
                // For files > 16 MB the engine's content_hash is a head+tail+size
                // COMPOSITE, not a full BLAKE3 — matching composites are "likely
                // duplicates", not byte-for-byte verified. Mark the group so the
                // caption drops the false "identical" guarantee that drives the
                // unsafe one-click delete (#3).
                IsApproximate = rawMembers[indices[0]].Size > FullHashMaxBytes,
            });
        }

        groups.Sort((a, b) => b.MemberCount.CompareTo(a.MemberCount));
        if (groups.Count > 200) groups.RemoveRange(200, groups.Count - 200);
        return groups;
    }

    private void ApplyOnUi(IReadOnlyList<DuplicateGroup> rows)
    {
        if (_ui.HasThreadAccess) Replace(rows);
        else _ui.TryEnqueue(() => Replace(rows));
    }

    private void Replace(IReadOnlyList<DuplicateGroup> rows)
        => MergeByContentHash(Groups, rows);

    /// <summary>Reconcile <paramref name="groups"/> to match <paramref name="rows"/>
    /// by <see cref="DuplicateGroup.ContentHash"/>, in place (mirrors
    /// <c>LibraryViewModel.MergeById</c>). The old Clear+Add raised a
    /// CollectionChanged.Reset ~1 Hz during a scan, re-realizing the whole
    /// ItemsRepeater, re-decoding every member thumbnail, and discarding the
    /// user's in-flight keeper/skip state. Surviving groups whose membership is
    /// unchanged keep their existing instance (and its IsKeeper / IsSkipped /
    /// loaded thumbnails); only genuine deltas emit Add/Remove. A group whose
    /// member set changed is replaced (its <c>Members</c> binding is OneTime, so
    /// the list must re-realize to reflect the new membership). Static +
    /// collection-only so it carries no UI-thread affinity beyond the
    /// ObservableCollection it mutates.</summary>
    internal static void MergeByContentHash(
        ObservableCollection<DuplicateGroup> groups,
        IReadOnlyList<DuplicateGroup> rows)
    {
        if (groups.Count == 0)
        {
            foreach (var r in rows) groups.Add(r);
            return;
        }

        var existingByHash = new Dictionary<string, DuplicateGroup>(groups.Count);
        foreach (var g in groups) existingByHash[g.ContentHash] = g;

        // Target sequence: reuse a surviving group instance only when its member
        // set is identical (so the OneTime Members binding stays valid and the
        // keeper/skip state is preserved); otherwise take the fresh instance.
        // `reused` tracks the surviving instances we keep by reference, so step 1
        // can drop the old instance of a group whose membership changed (its hash
        // survives but we're replacing it with the fresh one).
        var desired = new List<DuplicateGroup>(rows.Count);
        var nextHashes = new HashSet<string>(rows.Count);
        var reused = new HashSet<DuplicateGroup>();
        foreach (var fresh in rows)
        {
            if (!nextHashes.Add(fresh.ContentHash)) continue;
            if (existingByHash.TryGetValue(fresh.ContentHash, out var keep)
                && SameMembers(keep, fresh))
            {
                reused.Add(keep);
                desired.Add(keep);
            }
            else
            {
                desired.Add(fresh);
            }
        }

        // 1) Remove any existing group we're not reusing by reference — both
        //    genuinely-gone hashes and replaced-instance survivors.
        for (int i = groups.Count - 1; i >= 0; i--)
        {
            if (!reused.Contains(groups[i])) groups.RemoveAt(i);
        }

        // 2) Align order to `desired` via Remove+Insert of the instance, so a
        //    surviving-but-reordered group keeps its instance.
        for (int j = 0; j < desired.Count; j++)
        {
            var want = desired[j];
            if (j < groups.Count && ReferenceEquals(groups[j], want)) continue;
            int cur = IndexOfInstance(groups, want, j);
            if (cur >= 0) groups.RemoveAt(cur);
            groups.Insert(j, want);
        }
    }

    /// <summary>True when two groups hold the same member Ids (order-insensitive).
    /// Same ContentHash + same member set ⇒ the surviving instance is reusable
    /// and its keeper/skip state worth preserving.</summary>
    private static bool SameMembers(DuplicateGroup a, DuplicateGroup b)
    {
        if (a.Members.Count != b.Members.Count) return false;
        var ids = new HashSet<long>(a.Members.Count);
        foreach (var m in a.Members) ids.Add(m.Id);
        foreach (var m in b.Members) if (!ids.Contains(m.Id)) return false;
        return true;
    }

    private static int IndexOfInstance(
        ObservableCollection<DuplicateGroup> groups,
        DuplicateGroup want,
        int startAt)
    {
        for (int i = startAt; i < groups.Count; i++)
        {
            if (ReferenceEquals(groups[i], want)) return i;
        }
        return -1;
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name ?? string.Empty));
}

internal sealed class DuplicateGroup : INotifyPropertyChanged
{
    /// <summary>The shared content hash (BLAKE3 / composite, hex) of every
    /// member — the group's identity. Bound as the keeper RadioButton's Tag.</summary>
    public required string ContentHash { get; init; }
    public required IReadOnlyList<DuplicateMember> Members { get; init; }
    public int MemberCount => Members.Count;

    /// <summary>True when members exceed the engine's full-hash threshold, so
    /// the shared content_hash is a head+tail+size composite — "likely", not
    /// byte-verified duplicates. Drives the cautious caption (#3).</summary>
    public bool IsApproximate { get; init; }

    // FEAT-CRIT-2: per-group skip flag. Members of a skipped group are
    // excluded from "Trash non-keepers". Mirrors the macOS Cleanup
    // per-group "Skip" action.
    private bool _isSkipped;
    public bool IsSkipped
    {
        get => _isSkipped;
        set
        {
            if (_isSkipped == value) return;
            _isSkipped = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsSkipped)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Caption)));
        }
    }

    public string Caption
    {
        get
        {
            // Approximate (>16 MB composite-hash) groups are NOT byte-verified —
            // present them as "likely duplicates — verify before deleting" so the
            // caption never makes a false byte-for-byte guarantee (#3).
            var label = IsApproximate
                ? $"{MemberCount} likely duplicates — verify before deleting · {ShortHash}"
                : $"{MemberCount} identical copies · {ShortHash}";
            return IsSkipped ? $"{label} · SKIPPED" : label;
        }
    }

    /// <summary>First 12 chars of the content hash for a compact caption.</summary>
    private string ShortHash =>
        ContentHash.Length > 12 ? ContentHash[..12] : ContentHash;

    public event PropertyChangedEventHandler? PropertyChanged;
}

internal sealed class DuplicateMember : INotifyPropertyChanged
{
    public required long Id { get; init; }
    public required string Path { get; init; }
    public required string FileName { get; init; }
    public required long SizeBytes { get; init; }

    /// <summary>shared per-group key for the keeper RadioButton's
    /// GroupName. Was previously bound to `Path` per member, which made
    /// mutual exclusion impossible (each member had its own group). Set
    /// to the parent group's content hash at construction.</summary>
    public required string GroupKey { get; init; }

    public string SizeDisplay
    {
        get
        {
            var b = SizeBytes;
            if (b < 1024) return $"{b} B";
            if (b < 1024 * 1024) return $"{b / 1024.0:0.#} KB";
            if (b < 1024L * 1024 * 1024) return $"{b / (1024.0 * 1024):0.#} MB";
            return $"{b / (1024.0 * 1024 * 1024):0.##} GB";
        }
    }

    private bool _isKeeper;
    public bool IsKeeper
    {
        get => _isKeeper;
        set
        {
            if (_isKeeper == value) return;
            _isKeeper = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsKeeper)));
        }
    }

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _thumbnail;
    /// <summary>Shell thumbnail, loaded lazily by the view's members
    /// ItemsRepeater (ElementPrepared) via ThumbnailService — mirrors macOS's
    /// per-tile QLThumbnail. Null until loaded; cleared on tile recycle.</summary>
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? Thumbnail
    {
        get => _thumbnail;
        set
        {
            if (IsDetached) return;
            if (ReferenceEquals(_thumbnail, value)) return;
            _thumbnail = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowPlaceholder)));
        }
    }

    /// <summary>Placeholder-glyph visibility — shown until the thumbnail loads.</summary>
    public Microsoft.UI.Xaml.Visibility ShowPlaceholder =>
        _thumbnail == null ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;

    /// <summary>Marker set when the tile recycles out of the repeater so a late
    /// thumbnail bind can't land on a stale tile.</summary>
    public bool IsDetached { get; set; }

    /// <summary>Release the bound bitmap on recycle (bypasses the IsDetached
    /// guard so the recycled tile shows the placeholder, not a stale image).</summary>
    public void ClearThumbnailForRecycle()
    {
        if (_thumbnail == null) return;
        _thumbnail = null;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowPlaceholder)));
    }

    public event PropertyChangedEventHandler? PropertyChanged;
}
