// CleanupViewModel — backs the Cleanup tab duplicate groups list.
//
// Mirror of macOS app/Sources/FileID/Cleanup/CleanupViewModel.swift.
// Groups files by matching `phash` (perceptual hash, 64-bit) to find
// near-duplicate images. Each group lets the user mark one keeper and
// trash the others (engine `trashFiles` IPC command, parallel
// IFileOperation::DeleteItem with FOF_ALLOWUNDO).

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

internal sealed class CleanupViewModel : INotifyPropertyChanged
{
    private readonly string _dbPath;
    private readonly DispatcherQueue _ui;
    private bool _isLoading;
    private string? _errorMessage;

    public CleanupViewModel(string dbPath, DispatcherQueue ui)
    {
        _dbPath = dbPath;
        _ui = ui;
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
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            var groups = await Task.Run(() => Load(ct), ct).ConfigureAwait(false);
            ApplyOnUi(groups);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (Exception ex) { ErrorMessage = ex.Message; }
        finally { IsLoading = false; }
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
        // Group files by exact phash match. Phase 4.x extends to fuzzy
        // matches via Hamming distance ≤ 4 bits using the same approach
        // macOS uses (64-bit popcount on XOR of hash pairs).
        using var cmd = conn.CreateCommand();
        // Pull every image with a phash, then group:
        //   1. Exact-phash matches (cheap — straight equality).
        //   2. Near-matches via Hamming distance ≤ 4 bits (fuzzy).
        // The fuzzy pass is O(n²) on the in-memory candidate list but
        // we cap at 5000 phashes so worst-case 12.5M XOR-popcounts → ~100ms.
        cmd.CommandText = """
            SELECT id, path_text, size_bytes, phash
            FROM files
            WHERE phash IS NOT NULL AND kind = 'image'
            ORDER BY phash
            LIMIT 5000
            """;
        var rawMembers = new List<(long Id, string Path, long Size, long Phash)>(2048);
        using (var reader = cmd.ExecuteReader())
        {
            while (reader.Read())
            {
                ct.ThrowIfCancellationRequested();
                rawMembers.Add((reader.GetInt64(0), reader.GetString(1), reader.GetInt64(2), reader.GetInt64(3)));
            }
        }

        var groups = new List<DuplicateGroup>();
        // Use a union-find structure over indices to merge near-matches
        // into clusters. Two phashes belong to the same cluster if their
        // popcount(XOR) ≤ FuzzyThreshold.
        const int FuzzyThreshold = 4;
        int n = rawMembers.Count;
        var parent = new int[n];
        for (int i = 0; i < n; i++) parent[i] = i;
        int Find(int x) { while (parent[x] != x) { parent[x] = parent[parent[x]]; x = parent[x]; } return x; }
        void Union(int a, int b) { int ra = Find(a), rb = Find(b); if (ra != rb) parent[ra] = rb; }

        for (int i = 0; i < n; i++)
        {
            ct.ThrowIfCancellationRequested();
            for (int j = i + 1; j < n; j++)
            {
                long xor = rawMembers[i].Phash ^ rawMembers[j].Phash;
                if (System.Numerics.BitOperations.PopCount((ulong)xor) <= FuzzyThreshold)
                {
                    Union(i, j);
                }
            }
        }

        var byRoot = new Dictionary<int, List<int>>();
        for (int i = 0; i < n; i++)
        {
            int r = Find(i);
            if (!byRoot.TryGetValue(r, out var list)) { list = new List<int>(); byRoot[r] = list; }
            list.Add(i);
        }

        foreach (var (_, indices) in byRoot)
        {
            if (indices.Count < 2) continue;
            // Pick the largest file as default keeper (best resolution
            // typically). User can re-pick in the UI.
            indices.Sort((a, b) => rawMembers[b].Size.CompareTo(rawMembers[a].Size));
            var phash = rawMembers[indices[0]].Phash;
            // V14.7.6: shared GroupName for the keeper RadioButton so
            // mutual exclusion within a duplicate group works. Hex
            // representation of the perceptual hash uniquely identifies
            // the group across the whole tab.
            var groupKey = $"dup-{phash:X16}";
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
                PerceptualHash = phash,
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
    public required long PerceptualHash { get; init; }
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
            ? $"{MemberCount} duplicates · phash {PerceptualHash:X16} · SKIPPED"
            : $"{MemberCount} duplicates · phash {PerceptualHash:X16}";

    public event PropertyChangedEventHandler? PropertyChanged;
}

internal sealed class DuplicateMember : INotifyPropertyChanged
{
    public required long Id { get; init; }
    public required string Path { get; init; }
    public required string FileName { get; init; }
    public required long SizeBytes { get; init; }

    /// <summary>V14.7.6: shared per-group key for the keeper RadioButton's
    /// GroupName. Was previously bound to `Path` per member, which made
    /// mutual exclusion impossible (each member had its own group). Set
    /// to the parent group's perceptual hash hex string at construction.</summary>
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

    public event PropertyChangedEventHandler? PropertyChanged;
}
