// PeopleViewModel — backs the People tab cluster grid.
//
// Mirror of macOS app/Sources/FileID/People/PeopleViewModel.swift. Each
// cluster has a representative face image, a member count, an optional
// person name (set by the user), and a list of file IDs that contain
// faces in this cluster. The view shows them as cards in a wrap layout;
// tapping a card opens the PersonDetailSheet (Phase 3.x).

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

internal sealed class PeopleViewModel : INotifyPropertyChanged
{
    private readonly string _dbPath;
    private readonly DispatcherQueue _ui;
    private bool _isLoading;
    private string? _errorMessage;
    // FEAT-CRIT-1: multi-select mode for bulk merge / mark-as-unknown.
    private bool _isSelectMode;

    public PeopleViewModel(string dbPath, DispatcherQueue ui)
    {
        _dbPath = dbPath;
        _ui = ui;
    }

    public ObservableCollection<PersonCluster> Clusters { get; } = new();

    /// <summary>FEAT-CRIT-1: when true, cluster cards show a checkbox
    /// overlay and the bulk-action toolbar replaces the page header.</summary>
    public bool IsSelectMode
    {
        get => _isSelectMode;
        set
        {
            if (_isSelectMode == value) return;
            _isSelectMode = value;
            // Leaving select mode clears every selection.
            if (!value)
            {
                foreach (var c in Clusters) c.IsSelected = false;
            }
            OnPropertyChanged();
            OnPropertyChanged(nameof(SelectedCount));
        }
    }

    /// <summary>FEAT-CRIT-1: count of currently-selected cluster cards.</summary>
    public int SelectedCount
    {
        get
        {
            int n = 0;
            foreach (var c in Clusters) if (c.IsSelected) n++;
            return n;
        }
    }

    /// <summary>FEAT-CRIT-1: cluster IDs of every selected card, in order.</summary>
    public IReadOnlyList<long> SelectedClusterIds
    {
        get
        {
            var ids = new List<long>();
            foreach (var c in Clusters) if (c.IsSelected) ids.Add(c.ClusterId);
            return ids;
        }
    }

    public void NotifySelectedCountChanged() => OnPropertyChanged(nameof(SelectedCount));

    public bool IsLoading
    {
        get => _isLoading;
        private set
        {
            if (_isLoading == value) return;
            _isLoading = value;
            OnPropertyChanged();
        }
    }

    public string? ErrorMessage
    {
        get => _errorMessage;
        private set
        {
            if (_errorMessage == value) return;
            _errorMessage = value;
            OnPropertyChanged();
        }
    }

    public async Task RefreshAsync(CancellationToken ct)
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            var clusters = await Task.Run(() => LoadClusters(ct), ct).ConfigureAwait(false);
            ApplyOnUi(clusters);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (Exception ex)
        {
            ErrorMessage = ex.Message;
        }
        finally
        {
            IsLoading = false;
        }
    }

    private List<PersonCluster> LoadClusters(CancellationToken ct)
    {
        // First-launch guard: the engine creates the DB on first scan.
        if (!File.Exists(_dbPath))
        {
            return new List<PersonCluster>();
        }
        var connString = new SqliteConnectionStringBuilder
        {
            DataSource = _dbPath,
            Mode = SqliteOpenMode.ReadOnly,
        }.ToString();
        using var conn = new SqliteConnection(connString);
        conn.Open();
        using var cmd = conn.CreateCommand();
        // Cluster shape: face_prints (one row per detected face) joins
        // persons (one row per cluster). Display name = explicit `name`
        // (legacy free-form) → `first_name` (v5 structured) → fallback
        // "Person N". Anchor face = the face_prints row with the highest
        // quality score in the cluster — picked via subquery so it's stable.
        cmd.CommandText = """
            SELECT
                p.id                                                    AS cluster_id,
                COALESCE(p.name, p.first_name, 'Person ' || p.id)       AS display_name,
                COUNT(fp.id)                                            AS member_count,
                COALESCE(
                    p.representative_face_id,
                    (SELECT fp2.id FROM face_prints fp2
                     WHERE fp2.person_id = p.id
                     ORDER BY COALESCE(fp2.face_quality, 0) DESC LIMIT 1)
                )                                                       AS anchor_face_id
            FROM persons p
            JOIN face_prints fp ON fp.person_id = p.id
            GROUP BY p.id
            ORDER BY member_count DESC
            """;
        var rows = new List<PersonCluster>();
        using var reader = cmd.ExecuteReader();
        while (reader.Read())
        {
            ct.ThrowIfCancellationRequested();
            rows.Add(new PersonCluster
            {
                ClusterId = reader.GetInt32(0),
                DisplayName = reader.IsDBNull(1) ? null : reader.GetString(1),
                MemberCount = reader.GetInt32(2),
                AnchorFaceId = reader.IsDBNull(3) ? 0 : reader.GetInt64(3),
            });
        }
        return rows;
    }

    private void ApplyOnUi(IReadOnlyList<PersonCluster> rows)
    {
        if (_ui.HasThreadAccess)
        {
            Replace(rows);
        }
        else
        {
            _ui.TryEnqueue(() => Replace(rows));
        }
    }

    private void Replace(IReadOnlyList<PersonCluster> rows)
    {
        Clusters.Clear();
        foreach (var r in rows)
        {
            Clusters.Add(r);
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name ?? string.Empty));
}

internal sealed class PersonCluster : INotifyPropertyChanged
{
    public required int ClusterId { get; init; }
    public required long AnchorFaceId { get; init; }
    public required int MemberCount { get; init; }
    public string? DisplayName { get; init; }

    // FEAT-CRIT-1: per-card selection state for People multi-select.
    private bool _isSelected;
    public bool IsSelected
    {
        get => _isSelected;
        set
        {
            if (_isSelected == value) return;
            _isSelected = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsSelected)));
        }
    }
    public event PropertyChangedEventHandler? PropertyChanged;

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _cachedAnchorImage;
    private bool _anchorImageResolved;
    /// <summary>
    /// BitmapImage of the per-face JPEG written by the engine after
    /// ArcFace embed. Lazily constructed once + cached so the binding
    /// doesn't rebuild it on every refresh (which would flicker / loop).
    /// Null if the file doesn't exist (cluster from before V14.4 face-crop
    /// writes, or AnchorFaceId is 0).
    /// </summary>
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? AnchorImage
    {
        get
        {
            if (_anchorImageResolved) return _cachedAnchorImage;
            _anchorImageResolved = true;
            if (AnchorFaceId <= 0) return null;
            try
            {
                var path = System.IO.Path.Combine(Services.AppPaths.Root, "face_crops", $"{AnchorFaceId}.jpg");
                if (!System.IO.File.Exists(path)) return null;
                _cachedAnchorImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(path));
                return _cachedAnchorImage;
            }
            catch
            {
                return null;
            }
        }
    }

    public string Caption =>
        string.IsNullOrEmpty(DisplayName)
            ? $"Cluster {ClusterId} · {MemberCount} photo{(MemberCount == 1 ? string.Empty : "s")}"
            : $"{DisplayName} · {MemberCount}";
}
