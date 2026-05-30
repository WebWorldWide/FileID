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
        catch (Exception ex) { if (!_disposed) ErrorMessage = ex.Message; }
        finally { if (!_disposed) IsLoading = false; }
    }

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
    {
        Groups.Clear();
        foreach (var r in rows) Groups.Add(r);
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

    public string Caption =>
        IsSkipped
            ? $"{MemberCount} identical copies · {ShortHash} · SKIPPED"
            : $"{MemberCount} identical copies · {ShortHash}";

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
