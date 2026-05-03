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

    public PeopleViewModel(string dbPath, DispatcherQueue ui)
    {
        _dbPath = dbPath;
        _ui = ui;
    }

    public ObservableCollection<PersonCluster> Clusters { get; } = new();

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

internal sealed class PersonCluster
{
    public required int ClusterId { get; init; }
    public required long AnchorFaceId { get; init; }
    public required int MemberCount { get; init; }
    public string? DisplayName { get; init; }

    public string Caption =>
        string.IsNullOrEmpty(DisplayName)
            ? $"Cluster {ClusterId} · {MemberCount} photo{(MemberCount == 1 ? string.Empty : "s")}"
            : $"{DisplayName} · {MemberCount}";
}
