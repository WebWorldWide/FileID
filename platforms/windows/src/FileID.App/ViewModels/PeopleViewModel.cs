// PeopleViewModel — backs the People tab cluster grid.

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
    private bool _isSelectMode;
    private bool _disposed;
    /// <summary>Cancelled in Dispose so any RefreshAsync running on a thread-pool thread unwinds before the view's connection state is torn down.</summary>
    private readonly CancellationTokenSource _disposalCts = new();
    private long _refreshGen;
    private int _activeLoads;

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

    public bool IsSelectMode
    {
        get => _isSelectMode;
        set
        {
            if (_isSelectMode == value) return;
            _isSelectMode = value;
            if (!value)
            {
                foreach (var c in Clusters) c.IsSelected = false;
            }
            OnPropertyChanged();
            OnPropertyChanged(nameof(SelectedCount));
        }
    }

    public int SelectedCount
    {
        get
        {
            int n = 0;
            foreach (var c in Clusters) if (c.IsSelected) n++;
            return n;
        }
    }

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
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            // Created inside the try so a Dispose() race becomes a clean teardown no-op instead of an ObjectDisposedException escaping to the caller.
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() => { IsLoading = true; ErrorMessage = null; });
            var clusters = await Task.Run(() => LoadClusters(token), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            ApplyOnUi(clusters, myGen);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex)
        {
            // ConfigureAwait(false) above resumes this continuation on a thread-pool thread; IsLoading/ErrorMessage raise PropertyChanged that drives x:Bind XAML writes (ProgressRing.IsActive, StatusText), so they must be marshaled to the captured UI thread — else a native fast-fail (RPC_E_WRONG_THREAD).
            OnUi(() => { if (!_disposed) ErrorMessage = ex.Message; });
        }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

    private List<PersonCluster> LoadClusters(CancellationToken ct)
    {
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
        // Anchor face = the face_prints row with the highest quality score in the cluster — picked via subquery so it's stable.
        bool hideUnknown = false;
        try { hideUnknown = AppViewModel.Instance.Settings.PeopleHideUnknown; } catch { /* default false */ }
        cmd.CommandText = ClusterQuery(hideUnknown);

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
                AnchorCacheRevision = PersonCluster.ReadCropRevision(
                    PersonCluster.BuildCropPath(reader.IsDBNull(3) ? 0 : reader.GetInt64(3)),
                    reader.IsDBNull(4) ? null : reader.GetString(4)),
            });
        }
        return rows;
    }

    internal static string ClusterQuery(bool hideUnknown)
    {
        string whereClause = hideUnknown ? "WHERE COALESCE(p.is_unknown, 0) = 0" : "";

        return $"""
            WITH active_faces AS (
                SELECT fp.* FROM face_prints fp JOIN files f ON f.id = fp.file_id
                WHERE COALESCE(fp.excluded, 0) = 0 AND f.failed = 0
            ), clusters AS (
                SELECT p.id AS cluster_id,
                       COALESCE(p.name, p.first_name, 'Person ' || p.id) AS display_name,
                       COUNT(fp.id) AS member_count,
                       COALESCE(
                           (SELECT picked.id FROM active_faces picked
                            WHERE picked.id = p.representative_face_id AND picked.person_id = p.id
                              AND COALESCE(picked.excluded, 0) = 0),
                           (SELECT fp2.id FROM active_faces fp2
                            WHERE fp2.person_id = p.id AND COALESCE(fp2.excluded, 0) = 0
                            ORDER BY COALESCE(fp2.face_quality, 0) DESC, fp2.id ASC LIMIT 1)
                       ) AS anchor_face_id
                FROM persons p
                JOIN active_faces fp ON fp.person_id = p.id AND COALESCE(fp.excluded, 0) = 0
                {whereClause}
                GROUP BY p.id
            )
            SELECT clusters.cluster_id, clusters.display_name, clusters.member_count, clusters.anchor_face_id,
                   COALESCE(anchor.bbox,'') || '|' || COALESCE(anchor.embedding_model,'') || '|' ||
                   COALESCE(anchor.processing_version,'') || '|' || COALESCE(anchor.source_revision,'')
            FROM clusters LEFT JOIN active_faces anchor ON anchor.id = clusters.anchor_face_id
            ORDER BY clusters.member_count DESC
            """;
    }

    internal static string DeferredFaceStatus(ulong? deferredFaces) => deferredFaces is > 0
        ? $"{deferredFaces} more face{(deferredFaces == 1 ? " is" : "s are")} waiting to be grouped; existing groups were kept."
        : string.Empty;

    private void ApplyOnUi(IReadOnlyList<PersonCluster> rows, long gen)
    {
        // Drop results superseded by a newer refresh; checked on the UI thread right before the swap so a refresh started during the dispatch gap is caught too.
        void Apply()
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            Replace(rows);
        }
        if (_ui.HasThreadAccess)
        {
            Apply();
        }
        else
        {
            _ui.TryEnqueue(Apply);
        }
    }

    /// <summary>Marshal a UI-affined mutation onto the captured dispatcher.</summary>
    private void OnUi(Action action)
    {
        if (_ui.HasThreadAccess) action();
        else _ui.TryEnqueue(() => { if (!_disposed) action(); });
    }

    private void Replace(IReadOnlyList<PersonCluster> rows)
    {
        MergeByClusterId(Clusters, rows);
        OnPropertyChanged(nameof(SelectedCount));
    }

    /// <summary>Surviving clusters whose anchor face is unchanged keep their existing instance (and its IsSelected + already-decoded AnchorImage); only genuine deltas emit Add/Remove.</summary>
    internal static void MergeByClusterId(
        ObservableCollection<PersonCluster> clusters,
        IReadOnlyList<PersonCluster> rows)
    {
        if (clusters.Count == 0)
        {
            foreach (var r in rows) clusters.Add(r);
            return;
        }

        var existingById = new Dictionary<int, PersonCluster>(clusters.Count);
        foreach (var c in clusters) existingById[c.ClusterId] = c;

        var desired = new List<PersonCluster>(rows.Count);
        var nextIds = new HashSet<int>(rows.Count);
        var reused = new HashSet<PersonCluster>();
        foreach (var fresh in rows)
        {
            if (!nextIds.Add(fresh.ClusterId)) continue;
            if (existingById.TryGetValue(fresh.ClusterId, out var keep)
                && keep.AnchorFaceId == fresh.AnchorFaceId
                && keep.MemberCount == fresh.MemberCount
                && keep.DisplayName == fresh.DisplayName
                && keep.AnchorCacheRevision == fresh.AnchorCacheRevision)
            {
                // The Caption is a OneTime x:Bind over the init-only DisplayName/MemberCount, so a rename or a member-count change must take the FRESH instance to re-render — otherwise the card shows a stale name/count.
                reused.Add(keep);
                desired.Add(keep);
            }
            else
            {
                desired.Add(fresh);
            }
        }

        for (int i = clusters.Count - 1; i >= 0; i--)
        {
            if (!reused.Contains(clusters[i])) clusters.RemoveAt(i);
        }

        for (int j = 0; j < desired.Count; j++)
        {
            var want = desired[j];
            if (j < clusters.Count && ReferenceEquals(clusters[j], want)) continue;
            int cur = IndexOfInstance(clusters, want, j);
            if (cur >= 0) clusters.RemoveAt(cur);
            clusters.Insert(j, want);
        }
    }

    private static int IndexOfInstance(
        ObservableCollection<PersonCluster> clusters,
        PersonCluster want,
        int startAt)
    {
        for (int i = startAt; i < clusters.Count; i++)
        {
            if (ReferenceEquals(clusters[i], want)) return i;
        }
        return -1;
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name ?? string.Empty));
}

internal sealed class PersonCluster : INotifyPropertyChanged
{
    public required int ClusterId { get; init; }
    public required long AnchorFaceId { get; init; }
    public string? AnchorCacheRevision { get; init; }
    public required int MemberCount { get; init; }
    public string? DisplayName { get; init; }

    public string DisplayNameWithCount => string.IsNullOrEmpty(DisplayName)
        ? $"Person #{ClusterId} ({MemberCount} faces)"
        : $"{DisplayName} ({MemberCount} faces)";

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
    /// <summary>Lazily constructed once + cached so the binding doesn't rebuild it on every refresh (which would flicker / loop).</summary>
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
                // The load already looked at the crop off the UI thread and folded the answer into AnchorCacheRevision; only an instance built without one has to ask the disk here.
                var presence = CropPresence(AnchorCacheRevision);
                if (presence == false) return null;
                if (presence is null && !System.IO.File.Exists(path)) return null;
                var key = string.Concat(
                    AnchorFaceId.ToString(System.Globalization.CultureInfo.InvariantCulture), "|", AnchorCacheRevision);
                if (AnchorCacheRevision is not null && s_anchorImages.TryGetValue(key, out var shared))
                {
                    _cachedAnchorImage = shared;
                    return shared;
                }
                // IgnoreImageCache stays on for the first decode because the engine rewrites a crop in place under the same file name; s_anchorImages then shares that decode for the same revision.
                _cachedAnchorImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage
                {
                    DecodePixelWidth = 120,
                    CreateOptions = Microsoft.UI.Xaml.Media.Imaging.BitmapCreateOptions.IgnoreImageCache,
                    UriSource = new Uri(path),
                };
                if (AnchorCacheRevision is not null)
                {
                    if (s_anchorImages.Count >= MaxSharedAnchorImages) s_anchorImages.Clear();
                    s_anchorImages[key] = _cachedAnchorImage;
                }
                return _cachedAnchorImage;
            }
            catch
            {
                return null;
            }
        }
    }

    private const int MaxSharedAnchorImages = 600;
    // Read and written only from the UI thread (the AnchorImage binding).
    private static readonly Dictionary<string, Microsoft.UI.Xaml.Media.Imaging.BitmapImage> s_anchorImages = new(StringComparer.Ordinal);

    internal static bool? CropPresence(string? revision)
    {
        if (string.IsNullOrEmpty(revision)) return null;
        if (revision.EndsWith("|missing", StringComparison.Ordinal)) return false;
        if (revision.EndsWith("|unavailable", StringComparison.Ordinal)) return null;
        return true;
    }

    public static string BuildCropPath(long faceId) =>
        System.IO.Path.Combine(Services.AppPaths.Root, "face_crops", $"{faceId}.jpg");

    internal static string ReadCropRevision(string path, string? provenance)
    {
        try
        {
            var file = new FileInfo(path);
            return file.Exists
                ? string.Concat(provenance, "|", file.Length.ToString(System.Globalization.CultureInfo.InvariantCulture),
                    ":", file.LastWriteTimeUtc.Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture))
                : string.Concat(provenance, "|missing");
        }
        catch
        {
            return string.Concat(provenance, "|unavailable");
        }
    }

    public string Caption =>
        string.IsNullOrEmpty(DisplayName)
            ? $"Cluster {ClusterId} · {MemberCount} photo{(MemberCount == 1 ? string.Empty : "s")}"
            : $"{DisplayName} · {MemberCount}";
}

/// <summary>Rendering through this VM + a DataTemplate is deliberate: the template resolves {ThemeResource} brushes natively and the ItemsRepeater recycles containers, so the sheet never indexes theme brushes off Application.Resources (KeyNotFoundException) nor rebuilds sibling UIElement subtrees per engine event (layout-pass fast-fail) — the two crashes the prior imperative BuildRow path hit.</summary>
internal sealed class MergeSuggestionVm : INotifyPropertyChanged
{
    public required FileID.IpcSchema.MergeSuggestion Model { get; init; }

    public long SourcePersonId => Model.SourcePersonId;
    public long DestinationPersonId => Model.DestinationPersonId;
    public long SourceAnchorFaceId => Model.SourceAnchorFaceId;
    public long DestinationAnchorFaceId => Model.DestinationAnchorFaceId;

    public string Title => $"Person #{Model.SourcePersonId} ({Model.SourceMemberCount} faces) & Person #{Model.DestinationPersonId} ({Model.DestinationMemberCount} faces)";

    public string SimilarityText => $"{Math.Min(100, Math.Max(0, (int)Math.Round(Model.Similarity * 100)))}% alike";

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _sourceFaceImage;
    private bool _sourceFaceResolved;
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? SourceFaceImage
        => ResolveFace(Model.SourceAnchorFaceId, ref _sourceFaceImage, ref _sourceFaceResolved);

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _destFaceImage;
    private bool _destFaceResolved;
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? DestFaceImage
        => ResolveFace(Model.DestinationAnchorFaceId, ref _destFaceImage, ref _destFaceResolved);

    // Constructed during x:Bind evaluation on the UI thread; File.Exists/try-guarded so a missing or corrupt crop degrades to the placeholder Border instead of throwing.
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
    public bool ActionsEnabled => !_isResolved && !_isBusy;

    private bool _isBusy;
    /// <summary>True while an engine merge/different action is in flight for this pair; disables both action buttons so a second click can't double-apply during the up-to-30s engine await.</summary>
    public bool IsBusy
    {
        get => _isBusy;
        set
        {
            if (_isBusy == value) return;
            _isBusy = value;
            OnChanged(nameof(IsBusy));
            OnChanged(nameof(ActionsEnabled));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
