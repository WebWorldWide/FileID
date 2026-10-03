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
    // Refresh coordination (mirrors LibraryViewModel A4/A5): RefreshAsync bumps
    // _refreshGen and captures it; the UI-marshaled apply discards a result whose
    // generation is no longer current, so a slow earlier Load (e.g. a pre-trash
    // scan snapshot) can't clobber the latest Groups. _activeLoads counts in-flight
    // refreshes so the spinner stays on until the LAST one finishes — an earlier
    // finally no longer clears IsLoading while a later overlapping RefreshAsync is
    // still loading.
    private long _refreshGen;
    private int _activeLoads;

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
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            // Linked token created inside the try: a Dispose() race after the
            // _disposed check makes _disposalCts.Token throw ObjectDisposedException,
            // caught below as a clean teardown no-op instead of escaping to the caller.
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() => { if (Interlocked.Read(ref _refreshGen) == myGen) { IsLoading = true; ErrorMessage = null; } });
            var groups = await Task.Run(() => Load(token), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            ApplyOnUi(groups, myGen);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        // Surface DB/IO failures as an actionable message instead of the raw
        // SQLite jargon ("database disk image is malformed") the user can't act on.
        // ConfigureAwait(false) above resumes these catch/finally arms on a
        // thread-pool thread; ErrorMessage/IsLoading raise PropertyChanged that
        // drives x:Bind XAML writes (ProgressRing.IsActive, StatusText), so marshal
        // them to the captured UI thread — else a native fast-fail
        // (RPC_E_WRONG_THREAD). Mirrors LibraryViewModel.
        catch (SqliteException ex) { ReportLoadError(SqliteErrorTranslator.Humanize(ex), myGen); }
        catch (IOException ex) { ReportLoadError(SqliteErrorTranslator.Humanize(ex), myGen); }
        catch (Exception ex) { ReportLoadError(ex.Message, myGen); }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

    private void ReportLoadError(string message, long generation)
        => OnUi(() =>
        {
            if (_disposed || Interlocked.Read(ref _refreshGen) != generation) return;
            MergeByContentHash(Groups, Array.Empty<DuplicateGroup>());
            ErrorMessage = message;
        });

    /// Marshal a UI-affined mutation onto the captured dispatcher. RefreshAsync's
    /// catch/finally run on a thread-pool thread (Task.Run + ConfigureAwait(false)),
    /// so raising ErrorMessage/IsLoading PropertyChanged there would drive x:Bind
    /// XAML writes off the UI thread — a native fast-fail. No-op when already on the
    /// UI thread. Mirrors LibraryViewModel.OnUi.
    private void OnUi(Action action)
    {
        if (_ui.HasThreadAccess) action();
        else _ui.TryEnqueue(() => { if (!_disposed) action(); });
    }

    /// <summary>Files larger than this use a head+tail+size COMPOSITE
    /// content_hash in the engine, not a full BLAKE3 — so matching hashes are
    /// "likely", not byte-verified. Mirror of the engine's FULL_HASH_MAX_BYTES.</summary>
    private const long FullHashMaxBytes = 16L * 1024 * 1024;

    private List<DuplicateGroup> Load(CancellationToken ct)
        => LoadExactFromPath(_dbPath, ct);

    internal static List<DuplicateGroup> LoadExactFromPath(string dbPath, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        if (!File.Exists(dbPath)) return new List<DuplicateGroup>();

        var connString = new SqliteConnectionStringBuilder
        {
            DataSource = dbPath,
            Mode = SqliteOpenMode.ReadOnly,
        }.ToString();

        using var conn = new SqliteConnection(connString);
        conn.Open();
        using var cmd = conn.CreateCommand();
        cmd.CommandText = """
            WITH duplicate_keys AS (
                SELECT content_hash, size_bytes, COUNT(*) AS member_count
                FROM files
                WHERE content_hash IS NOT NULL AND length(content_hash) > 0 AND failed = 0
                GROUP BY content_hash, size_bytes
                HAVING COUNT(*) > 1
                ORDER BY member_count DESC, hex(content_hash), size_bytes
                LIMIT 200
            )
            SELECT f.id, f.path_text, f.size_bytes, f.content_hash, f.modified_at
            FROM duplicate_keys k
            JOIN files f ON f.content_hash = k.content_hash
                        AND f.size_bytes = k.size_bytes
            WHERE f.failed = 0
            ORDER BY k.member_count DESC, hex(k.content_hash), k.size_bytes,
                     COALESCE(f.aesthetic, -1) DESC,
                     COALESCE(f.created_at, 1e100) ASC,
                     f.path_text COLLATE BINARY
            """;

        var byKey = new Dictionary<string, List<DuplicateMember>>();
        using var reader = cmd.ExecuteReader();
        while (reader.Read())
        {
            ct.ThrowIfCancellationRequested();
            var hashBytes = (byte[])reader[3];
            if (hashBytes.Length == 0) continue;
            var hash = Convert.ToHexString(hashBytes);
            var size = reader.GetInt64(2);
            var key = $"{hash}:{size}";
            if (!byKey.TryGetValue(key, out var members))
            {
                members = new List<DuplicateMember>();
                byKey.Add(key, members);
            }
            var path = reader.GetString(1);
            members.Add(new DuplicateMember
            {
                Id = reader.GetInt64(0),
                Path = path,
                FileName = System.IO.Path.GetFileName(path),
                SizeBytes = size,
                ModifiedAt = reader.IsDBNull(4) ? null : reader.GetDouble(4),
                GroupKey = $"dup-{key}",
                IsKeeper = members.Count == 0,
            });
        }

        var groups = new List<DuplicateGroup>(byKey.Count);
        foreach (var (key, members) in byKey)
        {
            groups.Add(new DuplicateGroup
            {
                ContentHash = key,
                Members = members,
                IsApproximate = members[0].SizeBytes > FullHashMaxBytes,
            });
        }
        groups.Sort((a, b) =>
        {
            var countOrder = b.MemberCount.CompareTo(a.MemberCount);
            return countOrder != 0
                ? countOrder
                : string.CompareOrdinal(a.ContentHash, b.ContentHash);
        });
        return groups;
    }
    private void ApplyOnUi(IReadOnlyList<DuplicateGroup> rows, long gen)
    {
        // Drop results from a refresh a newer one has already superseded — checked
        // on the UI thread right before the swap so it also catches a refresh that
        // started during the dispatch gap. (mirrors LibraryViewModel A4)
        void Apply()
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            Replace(rows);
        }
        if (_ui.HasThreadAccess) Apply();
        else _ui.TryEnqueue(Apply);
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
    public int TotalMemberCount => Members.Count;
    public bool IsSimilar { get; init; }

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

    /// <summary>Modified-at unix seconds. Part of the thumbnail cache key so a
    /// member shown in both Cleanup and Library resolves to the same path|mtime
    /// L1/L2 entry instead of being cached twice.</summary>
    public double? ModifiedAt { get; init; }

    /// <summary>shared per-group key for the keeper RadioButton's
    /// GroupName. Was previously bound to `Path` per member, which made
    /// mutual exclusion impossible (each member had its own group). Set
    /// to the parent group's content hash at construction.</summary>
    public required string GroupKey { get; init; }
    public bool IsSimilar { get; init; }

    private bool _isSelectedForTrash;
    public bool IsSelectedForTrash
    {
        get => _isSelectedForTrash;
        set
        {
            if (_isSelectedForTrash == value) return;
            _isSelectedForTrash = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsSelectedForTrash)));
        }
    }

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
