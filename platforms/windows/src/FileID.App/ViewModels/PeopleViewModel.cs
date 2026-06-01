// PeopleViewModel — backs the People tab cluster grid.
//
// Each cluster has a representative face image, a member count, an
// optional person name (set by the user), and a list of file IDs that
// contain faces in this cluster. The view shows them as cards in a wrap
// layout; tapping a card opens the PersonDetailSheet.

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

internal sealed class PeopleViewModel : INotifyPropertyChanged, IDisposable
{
    private readonly string _dbPath;
    private readonly DispatcherQueue _ui;
    private bool _isLoading;
    private string? _errorMessage;
    // FEAT-CRIT-1: multi-select mode for bulk merge / mark-as-unknown.
    private bool _isSelectMode;
    private bool _disposed;
    /// <summary>Cancelled in <see cref="Dispose"/> so any RefreshAsync running
    /// on a thread-pool thread unwinds before the view's connection state
    /// is torn down.</summary>
    private readonly CancellationTokenSource _disposalCts = new();

    public PeopleViewModel(string dbPath, DispatcherQueue ui)
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
            var clusters = await Task.Run(() => LoadClusters(token), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            ApplyOnUi(clusters);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex)
        {
            if (!_disposed) ErrorMessage = ex.Message;
        }
        finally
        {
            if (!_disposed) IsLoading = false;
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
    /// Null if the file doesn't exist or AnchorFaceId is 0.
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
                var path = BuildCropPath(AnchorFaceId);
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

    /// <summary>Resolve the absolute path of the per-face JPEG the engine
    /// writes after ArcFace embed. Pure-function helper so test code can
    /// assert the path shape without depending on the cache state.</summary>
    public static string BuildCropPath(long faceId) =>
        System.IO.Path.Combine(Services.AppPaths.Root, "face_crops", $"{faceId}.jpg");

    public string Caption =>
        string.IsNullOrEmpty(DisplayName)
            ? $"Cluster {ClusterId} · {MemberCount} photo{(MemberCount == 1 ? string.Empty : "s")}"
            : $"{DisplayName} · {MemberCount}";
}

/// <summary>
/// Backs one row of the Suggested-merges sheet. Wraps an IPC
/// <see cref="FileID.IpcSchema.MergeSuggestion"/> and exposes display strings,
/// the two anchor-face thumbnails (lazily built + cached on the UI thread,
/// same pattern as <see cref="PersonCluster.AnchorImage"/>), and a resolved
/// flag that dims the row + disables its buttons once the user has acted.
///
/// Rendering through this VM + a DataTemplate is deliberate: the template
/// resolves {ThemeResource} brushes natively and the ItemsRepeater recycles
/// containers, so the sheet never indexes theme brushes off
/// Application.Resources (KeyNotFoundException) nor rebuilds sibling UIElement
/// subtrees per engine event (layout-pass fast-fail) — the two crashes the
/// prior imperative BuildRow path hit.
/// </summary>
internal sealed class MergeSuggestionVm : INotifyPropertyChanged
{
    public required FileID.IpcSchema.MergeSuggestion Model { get; init; }

    public long SourcePersonId => Model.SourcePersonId;
    public long DestinationPersonId => Model.DestinationPersonId;
    public long SourceAnchorFaceId => Model.SourceAnchorFaceId;
    public long DestinationAnchorFaceId => Model.DestinationAnchorFaceId;

    public string Title =>
        $"#{Model.SourcePersonId} ({Model.SourceMemberCount}) ↔ #{Model.DestinationPersonId} ({Model.DestinationMemberCount})";

    public string SimilarityText => $"Similarity {Model.Similarity:F2}";

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _sourceFaceImage;
    private bool _sourceFaceResolved;
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? SourceFaceImage
        => ResolveFace(Model.SourceAnchorFaceId, ref _sourceFaceImage, ref _sourceFaceResolved);

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _destFaceImage;
    private bool _destFaceResolved;
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? DestFaceImage
        => ResolveFace(Model.DestinationAnchorFaceId, ref _destFaceImage, ref _destFaceResolved);

    // Lazily build + cache the per-face JPEG. Constructed during x:Bind
    // evaluation on the UI thread; File.Exists/try-guarded so a missing or
    // corrupt crop degrades to the placeholder Border instead of throwing.
    // DecodePixelWidth caps the decode at the 80px display size.
    private static Microsoft.UI.Xaml.Media.Imaging.BitmapImage? ResolveFace(
        long faceId,
        ref Microsoft.UI.Xaml.Media.Imaging.BitmapImage? cache,
        ref bool resolved)
    {
        if (resolved) return cache;
        resolved = true;
        if (faceId <= 0) return null;
        try
        {
            var path = PersonCluster.BuildCropPath(faceId);
            if (!System.IO.File.Exists(path)) return null;
            cache = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage
            {
                DecodePixelWidth = 80,
                UriSource = new Uri(path),
            };
            return cache;
        }
        catch
        {
            return null;
        }
    }

    private bool _isResolved;
    /// <summary>True once the user has merged or marked-different this pair;
    /// dims the row and disables both action buttons.</summary>
    public bool IsResolved
    {
        get => _isResolved;
        set
        {
            if (_isResolved == value) return;
            _isResolved = value;
            OnChanged(nameof(IsResolved));
            OnChanged(nameof(RowOpacity));
            OnChanged(nameof(ActionsEnabled));
        }
    }
    public double RowOpacity => _isResolved ? 0.4 : 1.0;
    public bool ActionsEnabled => !_isResolved;

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
