// CleanupViewModel — backs the Cleanup tab duplicate groups list.
//
// Uses persisted `content_hash` only to find candidate groups. The current
// engine stores full SHA-256 through 16 MiB and a sampled SHA-256 identity above
// that size, so Exact deletion independently full-hashes the selected keeper and
// victims before sending keeper-bound proof to the engine.

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

/// <summary>Cleanup tab mode (macOS parity). Exact = persisted-hash candidates
/// that require live full-byte proof before mutation; Similar = visually near-identical images grouped by
/// dHash Hamming distance.</summary>
internal enum CleanupMode { Exact, Similar }

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

    // Cleanup has two modes (macOS parity): Exact shows content-hash candidate
    // groups whose selected files are live-verified before Trash; Similar groups visually
    // near-identical images by dHash Hamming distance (resizes / re-encodes /
    // crops / light edits). The view sets Mode, then calls RefreshAsync to reload.
    private CleanupMode _mode = CleanupMode.Exact;
    public CleanupMode Mode
    {
        get => _mode;
        set { if (_mode != value) { _mode = value; OnPropertyChanged(); } }
    }

    // Mode whose result currently populates Groups (UI-thread, set in ApplyOnUi).
    // A failed reload for a DIFFERENT mode must clear the list — leaving it would
    // render the old mode's groups under the new mode's header (e.g. Exact groups
    // mislabeled after LoadSimilar's >20k cap exception). Same-mode failures keep
    // the stale list so a transient DB error mid-scan doesn't wipe keeper/skip state.
    private CleanupMode _groupsMode = CleanupMode.Exact;

    public async Task RefreshAsync(CancellationToken ct)
    {
        if (_disposed) return;
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        // Snapshot the mode before fanning out to the thread pool so a mode
        // flip mid-refresh can't make the background Load read a torn value;
        // the generation guard in ApplyOnUi still discards the stale result.
        var mode = _mode;
        var displayedMode = _groupsMode;
        void ClearStaleModeGroups()
        {
            if (displayedMode != mode) ApplyOnUi(Array.Empty<DuplicateGroup>(), myGen, mode);
        }
        try
        {
            // Linked token created inside the try: a Dispose() race after the
            // _disposed check makes _disposalCts.Token throw ObjectDisposedException,
            // caught below as a clean teardown no-op instead of escaping to the caller.
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() => { IsLoading = true; ErrorMessage = null; });
            var groups = await Task.Run(() => Load(mode, token), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            ApplyOnUi(groups, myGen, mode);
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
        catch (SqliteException ex) { OnUi(() => { if (!_disposed) ErrorMessage = SqliteErrorTranslator.Humanize(ex); }); ClearStaleModeGroups(); }
        catch (IOException ex) { OnUi(() => { if (!_disposed) ErrorMessage = SqliteErrorTranslator.Humanize(ex); }); ClearStaleModeGroups(); }
        catch (Exception ex) { OnUi(() => { if (!_disposed) ErrorMessage = ex.Message; }); ClearStaleModeGroups(); }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

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

    /// <summary>Files larger than this use sampled SHA-256 content identity in
    /// the engine, not a full digest. Mirror of the engine's FULL_HASH_MAX_BYTES.</summary>
    private const long FullHashMaxBytes = 16L * 1024 * 1024;
    private const int MaxGroups = 200;
    private const int MaxVisibleMembers = 5_000;
    private const int MaxVisibleMembersPerGroup = 500;

    private List<DuplicateGroup> Load(CleanupMode mode, CancellationToken ct)
        => mode == CleanupMode.Similar ? LoadSimilar(ct) : LoadExact(ct);

    private List<DuplicateGroup> LoadExact(CancellationToken ct)
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
        // Let SQLite's indexed content_hash grouping find only the largest
        // duplicate groups. The old implementation materialized every hashed
        // file and a second dictionary even though the UI rendered 200 groups.
        // That made opening Cleanup proportional to the entire library.
        using var cmd = conn.CreateCommand();
        cmd.CommandText = """
            SELECT content_hash, size_bytes, COUNT(*) AS n
            FROM files
            WHERE content_hash IS NOT NULL AND failed = 0
            GROUP BY content_hash, size_bytes
            HAVING n > 1
            ORDER BY n DESC, hex(content_hash), size_bytes
            LIMIT $maxGroups
            """;
        cmd.Parameters.AddWithValue("$maxGroups", MaxGroups);
        var keys = new List<(byte[] Hash, long Size, int Count)>(MaxGroups);
        using (var reader = cmd.ExecuteReader())
        {
            while (reader.Read())
            {
                ct.ThrowIfCancellationRequested();
                if (reader.IsDBNull(0)) continue;
                var hashBytes = (byte[])reader[0];
                if (hashBytes.Length == 0) continue;
                keys.Add((hashBytes, reader.GetInt64(1), reader.GetInt32(2)));
            }
        }

        var groups = new List<DuplicateGroup>(keys.Count);
        var remaining = MaxVisibleMembers;
        foreach (var key in keys)
        {
            if (remaining < 2) break;
            var visible = Math.Min(Math.Min(key.Count, MaxVisibleMembersPerGroup), remaining);
            using var membersCmd = conn.CreateCommand();
            membersCmd.CommandText = """
                SELECT id, path_text, size_bytes, modified_at
                FROM files
                WHERE content_hash = $hash AND size_bytes = $size AND failed = 0
                ORDER BY COALESCE(aesthetic, 0) DESC,
                         size_bytes DESC,
                         COALESCE(created_at, 1e18) ASC,
                         LENGTH(path_text) ASC,
                         path_text ASC
                LIMIT $limit
                """;
            membersCmd.Parameters.AddWithValue("$hash", key.Hash);
            membersCmd.Parameters.AddWithValue("$size", key.Size);
            membersCmd.Parameters.AddWithValue("$limit", visible);
            var hash = Convert.ToHexString(key.Hash);
            var groupKey = $"dup-{hash}:{key.Size}";
            var members = new List<DuplicateMember>(visible);
            using (var memberReader = membersCmd.ExecuteReader())
            {
                while (memberReader.Read())
                {
                    ct.ThrowIfCancellationRequested();
                    var path = memberReader.GetString(1);
                    members.Add(new DuplicateMember
                    {
                        Id = memberReader.GetInt64(0),
                        Path = path,
                        FileName = System.IO.Path.GetFileName(path),
                        SizeBytes = memberReader.GetInt64(2),
                        ModifiedAt = memberReader.IsDBNull(3) ? null : memberReader.GetDouble(3),
                        GroupKey = groupKey,
                        IsKeeper = members.Count == 0,
                    });
                }
            }
            if (members.Count < 2) continue;
            groups.Add(new DuplicateGroup
            {
                ContentHash = hash,
                Members = members,
                TotalMemberCount = key.Count,
                IsApproximate = key.Size > FullHashMaxBytes,
            });
            remaining -= members.Count;
        }
        return groups;
    }

    // ─── Perceptual near-duplicate ("Visually similar") grouping ─────────────
    // Mirrors macOS ReadStore.similarImageGroups(maxHamming:). Images whose 64-bit
    // dHashes (files.phash) are within a Hamming threshold of one another are
    // transitively unioned into a group via PerceptualGrouping. Same keeper rank
    // as exact dupes. Pure byte-exact clusters are dropped — they already appear
    // under "Exact".

    /// <summary>Default Hamming threshold for "visually similar" grouping.
    /// FILEID_NEARDUP_HAMMING overrides; clamped to 0..20. 8 of 64 bits ≈ visually
    /// near-identical (resize / re-encode / crop / light edit) — deliberately tight
    /// so distinct photos of the same subject over time don't collapse into one
    /// group. (mirrors macOS ReadStore.defaultNearDupHamming)</summary>
    private static int NearDupHammingThreshold
    {
        get
        {
            var raw = Environment.GetEnvironmentVariable("FILEID_NEARDUP_HAMMING");
            if (!string.IsNullOrEmpty(raw) && int.TryParse(raw, out var v))
            {
                return Math.Min(20, Math.Max(0, v));
            }
            return 8;
        }
    }

    /// <summary>Above this image-with-dHash count the O(N²) pairwise scan is skipped
    /// rather than hang the UI. (mirrors macOS ReadStore.nearDupImageCap)</summary>
    private const int NearDupImageCap = 20_000;

    private List<DuplicateGroup> LoadSimilar(CancellationToken ct)
    {
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
        using (var countCmd = conn.CreateCommand())
        {
            countCmd.CommandText = """
                SELECT COUNT(*) FROM files
                WHERE kind = 'image' AND failed = 0
                  AND phash IS NOT NULL AND phash != 0
                """;
            var candidateCount = Convert.ToInt64(countCmd.ExecuteScalar());
            if (candidateCount > NearDupImageCap)
            {
                throw new InvalidOperationException(
                    $"Visually similar comparison is unavailable for {candidateCount:N0} images: " +
                    $"the exact Hamming matcher is capped at {NearDupImageCap:N0}. " +
                    "Exact duplicate cleanup remains available.");
            }
        }
        // Only images carry a dHash. phash == 0 is the engine's "none / failed"
        // sentinel (see the Rust dbwriter), so exclude it alongside NULL — else
        // every blank-hash image would collapse into one giant false group.
        // content_hash rides along so pure byte-exact clusters can be dropped;
        // created_at + aesthetic + size feed the keeper rank (macOS parity).
        using var cmd = conn.CreateCommand();
        cmd.CommandText = """
            SELECT id, path_text, size_bytes, content_hash, modified_at, created_at, aesthetic, phash
            FROM files
            WHERE kind = 'image' AND failed = 0
              AND phash IS NOT NULL AND phash != 0
            """;
        var rawMembers = new List<(long Id, string Path, long Size, byte[]? Hash, double? ModifiedAt, double? CreatedAt, double? Aesthetic, long Phash)>(2048);
        using (var reader = cmd.ExecuteReader())
        {
            while (reader.Read())
            {
                ct.ThrowIfCancellationRequested();
                if (reader.IsDBNull(7)) continue;
                var phash = reader.GetInt64(7);
                if (phash == 0) continue;
                var hashBytes = reader.IsDBNull(3) ? null : (byte[])reader[3];
                var modifiedAt = reader.IsDBNull(4) ? (double?)null : reader.GetDouble(4);
                var createdAt = reader.IsDBNull(5) ? (double?)null : reader.GetDouble(5);
                var aesthetic = reader.IsDBNull(6) ? (double?)null : reader.GetDouble(6);
                rawMembers.Add((reader.GetInt64(0), reader.GetString(1), reader.GetInt64(2), hashBytes, modifiedAt, createdAt, aesthetic, phash));
            }
        }

        if (rawMembers.Count <= 1) return new List<DuplicateGroup>();
        var indexById = new Dictionary<long, int>(rawMembers.Count);
        var items = new List<(long Id, long Phash)>(rawMembers.Count);
        for (int i = 0; i < rawMembers.Count; i++)
        {
            indexById[rawMembers[i].Id] = i;
            items.Add((rawMembers[i].Id, rawMembers[i].Phash));
        }

        var maxHamming = NearDupHammingThreshold;
        var groups = new List<DuplicateGroup>();
        var remaining = MaxVisibleMembers;
        foreach (var ids in PerceptualGrouping.GroupByHamming(items, maxHamming, ct))
        {
            ct.ThrowIfCancellationRequested();
            if (remaining < 2) break;
            var indices = new List<int>(ids.Count);
            foreach (var id in ids)
            {
                if (indexById.TryGetValue(id, out var idx)) indices.Add(idx);
            }
            if (indices.Count < 2) continue;

            // Drop pure byte-exact clusters — every member shares one non-null
            // content_hash — since they already appear under "Exact".
            bool allByteExact = true;
            string? firstHex = null;
            foreach (var idx in indices)
            {
                var h = rawMembers[idx].Hash;
                if (h == null || h.Length == 0) { allByteExact = false; break; }
                var hex = Convert.ToHexString(h);
                if (firstHex == null) { firstHex = hex; }
                else if (!string.Equals(firstHex, hex, StringComparison.Ordinal)) { allByteExact = false; break; }
            }
            if (allByteExact) continue;

            // Keeper rank (macOS parity): aesthetic DESC, size DESC, earliest
            // created_at ASC, shortest path ASC, then path ordinal as a stable
            // final tiebreaker. The member that sorts first is the default keeper.
            indices.Sort((a, b) =>
            {
                var ma = rawMembers[a];
                var mb = rawMembers[b];
                var aestheticCmp = (mb.Aesthetic ?? 0).CompareTo(ma.Aesthetic ?? 0);
                if (aestheticCmp != 0) return aestheticCmp;
                if (ma.Size != mb.Size) return mb.Size.CompareTo(ma.Size);
                var createdCmp = (ma.CreatedAt ?? double.MaxValue).CompareTo(mb.CreatedAt ?? double.MaxValue);
                if (createdCmp != 0) return createdCmp;
                if (ma.Path.Length != mb.Path.Length) return ma.Path.Length.CompareTo(mb.Path.Length);
                return string.CompareOrdinal(ma.Path, mb.Path);
            });

            // Stable group identity: the smallest member file id — independent of
            // which copy currently ranks as keeper, so a mid-scan re-rank doesn't
            // change the group key (and the identity-stable merge keeps the
            // instance + skip state). Mirrors macOS's gid = files.map(\.id).min().
            long gid = long.MaxValue;
            foreach (var idx in indices)
            {
                if (rawMembers[idx].Id < gid) gid = rawMembers[idx].Id;
            }
            var groupKey = $"sim-{gid}";
            var visible = Math.Min(Math.Min(indices.Count, MaxVisibleMembersPerGroup), remaining);
            var members = new List<DuplicateMember>(visible);
            for (int k = 0; k < visible; k++)
            {
                var m = rawMembers[indices[k]];
                members.Add(new DuplicateMember
                {
                    Id = m.Id,
                    Path = m.Path,
                    FileName = System.IO.Path.GetFileName(m.Path),
                    SizeBytes = m.Size,
                    ModifiedAt = m.ModifiedAt,
                    GroupKey = groupKey,
                    // A recommended keeper is still marked (so the per-group trash
                    // never targets the whole group), but the global "Trash
                    // non-keepers" bulk action is hidden in Similar mode — these
                    // are NOT byte-identical, so no one-click mass delete.
                    IsKeeper = k == 0,
                });
            }
            groups.Add(new DuplicateGroup
            {
                ContentHash = groupKey,
                Members = members,
                TotalMemberCount = indices.Count,
                IsApproximate = false,
                IsSimilar = true,
            });
            remaining -= members.Count;
        }

        // Largest clusters first, mirroring the exact view's ORDER BY n DESC.
        groups.Sort((a, b) => b.MemberCount.CompareTo(a.MemberCount));
        if (groups.Count > 200) groups.RemoveRange(200, groups.Count - 200);
        return groups;
    }

    private void ApplyOnUi(IReadOnlyList<DuplicateGroup> rows, long gen, CleanupMode mode)
    {
        // Drop results from a refresh a newer one has already superseded — checked
        // on the UI thread right before the swap so it also catches a refresh that
        // started during the dispatch gap. (mirrors LibraryViewModel A4)
        void Apply()
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            _groupsMode = mode;
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
        if (a.MemberCount != b.MemberCount) return false;
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
    /// <summary>The persisted full or sampled content identity shared by the
    /// candidate group. Bound as the keeper RadioButton's Tag.</summary>
    public required string ContentHash { get; init; }
    public required IReadOnlyList<DuplicateMember> Members { get; init; }
    public int TotalMemberCount { get; init; }
    public int MemberCount => TotalMemberCount > 0 ? TotalMemberCount : Members.Count;
    public bool IsTruncated => MemberCount > Members.Count;

    /// <summary>True when members exceed the engine's full-hash threshold, so
    /// the shared content_hash is sampled SHA-256 — "likely", not
    /// byte-verified duplicates. Drives the cautious caption (#3).</summary>
    public bool IsApproximate { get; init; }

    /// <summary>True for a perceptual ("Visually similar") group — its members are
    /// near-identical by dHash Hamming distance, NOT byte-for-byte identical. Drives
    /// the per-group "Visually similar" badge + the cautious caption. (macOS parity:
    /// DuplicateGroup.isSimilar)</summary>
    public bool IsSimilar { get; init; }

    /// <summary>Visibility of the per-group "Visually similar" caution badge.</summary>
    public Microsoft.UI.Xaml.Visibility SimilarBadgeVisibility =>
        IsSimilar ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;

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
            string label;
            if (IsSimilar)
            {
                // Perceptual group: the key is synthetic (no content hash to show).
                // Mirror the macOS "N images · review before deleting" framing so the
                // caption never implies a byte-for-byte guarantee.
                label = $"{MemberCount} visually similar image{(MemberCount == 1 ? "" : "s")} — review before deleting";
            }
            else
            {
                // Approximate (>16 MB sampled-hash) groups are NOT byte-verified —
                // present them as "likely duplicates — verify before deleting" so the
                // caption never makes a false byte-for-byte guarantee (#3).
                label = IsApproximate
                    ? $"{MemberCount} likely duplicates — verify before deleting · {ShortHash}"
                    : $"{MemberCount} identical copies · {ShortHash}";
            }
            if (IsTruncated) label += $" · showing {Members.Count}";
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
