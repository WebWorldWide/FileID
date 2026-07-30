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
    private readonly Func<bool> _hideUnknown;
    private bool _isLoading;
    private string? _errorMessage;
    // FEAT-CRIT-1: multi-select mode for bulk merge / mark-as-unknown.
    private bool _isSelectMode;
    private bool _disposed;
    /// <summary>Cancelled in <see cref="Dispose"/> so any RefreshAsync running
    /// on a thread-pool thread unwinds before the view's connection state
    /// is torn down.</summary>
    private readonly CancellationTokenSource _disposalCts = new();
    // Refresh coordination (mirrors LibraryViewModel A4/A5): every RefreshAsync
    // bumps _refreshGen and captures it; ApplyOnUi discards a result a newer
    // refresh has superseded, so a slow earlier load (e.g. a pre-merge DB read
    // enqueued by a faceClusteringComplete event) can't apply last and re-add a
    // merged-away cluster. _activeLoads keeps the spinner on until the LAST load
    // finishes, so an earlier finally no longer clears it prematurely.
    private long _refreshGen;
    private int _activeLoads;

    /// <summary>Minimum faces a cluster needs before it appears in the People
    /// grid (named clusters are always shown regardless — see the HAVING clause).
    ///
    /// Face clustering over-splits: on a real 135k-file library it produced 3,108
    /// clusters, of which 2,271 held 5 or fewer faces — overwhelmingly
    /// duplicate-burst fragments of one shot rather than distinct people. Showing
    /// all of them made the tab unusable ("thousands of leftover faces") and
    /// buried the few dozen clusters actually worth naming. 6 is the measured
    /// knee: clusters of >=6 faces have mean pairwise cosine 0.71-0.86 (coherent
    /// identities), and dropping to >=6 cut the surfaced count by roughly 3/4
    /// while keeping every large cluster.
    ///
    /// Fragments are NOT deleted and remain fully searchable — they are only
    /// held back from the grid, and any of them can still be reached by naming or
    /// through merge suggestions.</summary>
    public const int MinFacesPerCluster = 6;

    /// <summary>Count of clusters withheld by <see cref="MinFacesPerCluster"/> on
    /// the last refresh, so the view can disclose them instead of silently
    /// dropping them.</summary>
    public int HiddenSmallClusterCount
    {
        get => _hiddenSmallClusterCount;
        private set
        {
            if (_hiddenSmallClusterCount == value) return;
            _hiddenSmallClusterCount = value;
            OnPropertyChanged();
        }
    }
    private int _hiddenSmallClusterCount;

    public PeopleViewModel(string dbPath, DispatcherQueue ui, Func<bool>? hideUnknown = null)
    {
        _dbPath = dbPath;
        _ui = ui;
        _hideUnknown = hideUnknown ?? (() => false);
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
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            // Linked token created inside the try: a Dispose() race after the
            // _disposed check makes _disposalCts.Token throw ObjectDisposedException,
            // caught below as a clean teardown no-op instead of escaping to the caller.
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
            // ConfigureAwait(false) above resumes this continuation on a
            // thread-pool thread; IsLoading/ErrorMessage raise PropertyChanged that
            // drives x:Bind XAML writes (ProgressRing.IsActive, StatusText), so they
            // must be marshaled to the captured UI thread — else a native fast-fail
            // (RPC_E_WRONG_THREAD). Mirrors LibraryViewModel.
            //
            // Guard the write with the generation token: a slow FAILING refresh
            // must not overwrite a newer SUCCESSFUL refresh's cleared (null) error
            // with a stale banner.
            OnUi(() => { if (!_disposed && Interlocked.Read(ref _refreshGen) == myGen) ErrorMessage = ex.Message; });
        }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
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
                 AND COALESCE(fp.excluded, 0) = 0
            WHERE ($hide_unknown = 0 OR COALESCE(p.is_unknown, 0) = 0)
            GROUP BY p.id
            HAVING COUNT(fp.id) >= $min_faces
               -- A cluster the user has already named is always shown, however
               -- small: hiding someone's own labelled person would be wrong.
               OR (p.name IS NOT NULL AND TRIM(p.name) <> '')
               OR (p.first_name IS NOT NULL AND TRIM(p.first_name) <> '')
            ORDER BY member_count DESC
            """;
        cmd.Parameters.AddWithValue("$hide_unknown", _hideUnknown() ? 1 : 0);
        cmd.Parameters.AddWithValue("$min_faces", MinFacesPerCluster);
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
        reader.Close();

        // Count what the size floor withheld, so the view can disclose it rather
        // than silently hiding clusters. Same predicate as above, inverted.
        using var hiddenCmd = conn.CreateCommand();
        hiddenCmd.CommandText = """
            SELECT COUNT(*) FROM (
                SELECT p.id
                FROM persons p
                JOIN face_prints fp ON fp.person_id = p.id
                     AND COALESCE(fp.excluded, 0) = 0
                WHERE ($hide_unknown = 0 OR COALESCE(p.is_unknown, 0) = 0)
                GROUP BY p.id
                HAVING COUNT(fp.id) < $min_faces
                   AND (p.name IS NULL OR TRIM(p.name) = '')
                   AND (p.first_name IS NULL OR TRIM(p.first_name) = '')
            )
            """;
        hiddenCmd.Parameters.AddWithValue("$hide_unknown", _hideUnknown() ? 1 : 0);
        hiddenCmd.Parameters.AddWithValue("$min_faces", MinFacesPerCluster);
        var hidden = hiddenCmd.ExecuteScalar();
        _pendingHiddenCount = hidden is long l ? (int)l : Convert.ToInt32(hidden ?? 0);

        return rows;
    }

    /// Carried from the DB worker to the UI thread by ApplyOnUi, so
    /// HiddenSmallClusterCount is only published alongside its own row set.
    private int _pendingHiddenCount;

    private void ApplyOnUi(IReadOnlyList<PersonCluster> rows, long gen)
    {
        // Drop results from a refresh a newer one has already superseded — checked
        // on the UI thread right before the swap so it also catches a refresh that
        // started during the dispatch gap. Mirrors LibraryViewModel.ApplyOnUi (audit A4).
        void ApplySafely() => DebugLog.SafeRun("PeopleViewModel.ApplyOnUi", () =>
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            Replace(rows);
            HiddenSmallClusterCount = _pendingHiddenCount;
        });
        if (_ui.HasThreadAccess)
        {
            ApplySafely();
        }
        else
        {
            _ui.TryEnqueue(ApplySafely);
        }
    }

    /// Marshal a UI-affined mutation onto the captured dispatcher. RefreshAsync's
    /// catch/finally run on a thread-pool thread (Task.Run + ConfigureAwait(false)),
    /// so raising IsLoading/ErrorMessage PropertyChanged there would drive x:Bind
    /// XAML writes off the UI thread — a native fast-fail. No-op when already on the
    /// UI thread. Mirrors LibraryViewModel.OnUi.
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

    /// <summary>Reconcile <paramref name="clusters"/> to match <paramref name="rows"/>
    /// by <see cref="PersonCluster.ClusterId"/>, in place (mirrors
    /// <c>LibraryViewModel.MergeById</c>). The old Clear+Add raised a
    /// CollectionChanged.Reset ~1 Hz during a scan, re-realizing the whole
    /// ItemsRepeater, full-res-decoding every anchor face again, and discarding
    /// the user's in-flight multi-select state. Surviving clusters whose anchor
    /// face is unchanged keep their existing instance (and its IsSelected +
    /// already-decoded AnchorImage); only genuine deltas emit Add/Remove. A
    /// cluster whose anchor face changed is replaced so its OneTime AnchorImage /
    /// Caption bindings re-realize against the fresh crop. Static +
    /// collection-only so it carries no UI-thread affinity beyond the
    /// ObservableCollection it mutates.</summary>
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

        // `reused` tracks the surviving instances we keep by reference, so step 1
        // can drop the old instance of a cluster whose anchor face changed (its
        // ClusterId survives but we're replacing it with the fresh one).
        var desired = new List<PersonCluster>(rows.Count);
        var nextIds = new HashSet<int>(rows.Count);
        var reused = new HashSet<PersonCluster>();
        foreach (var fresh in rows)
        {
            if (!nextIds.Add(fresh.ClusterId)) continue;
            if (existingById.TryGetValue(fresh.ClusterId, out var keep)
                && keep.AnchorFaceId == fresh.AnchorFaceId
                && keep.MemberCount == fresh.MemberCount
                && keep.DisplayName == fresh.DisplayName)
            {
                // Reuse the instance (preserving IsSelected + the decoded
                // AnchorImage) ONLY when nothing visible changed. The Caption is a
                // OneTime x:Bind over the init-only DisplayName/MemberCount, so a
                // rename or a member-count change must take the FRESH instance to
                // re-render — otherwise the card shows a stale name/count.
                reused.Add(keep);
                desired.Add(keep);
            }
            else
            {
                desired.Add(fresh);
            }
        }

        // 1) Remove any existing cluster we're not reusing by reference — both
        //    genuinely-gone ids and replaced-instance survivors.
        for (int i = clusters.Count - 1; i >= 0; i--)
        {
            if (!reused.Contains(clusters[i])) clusters.RemoveAt(i);
        }

        // 2) Align order to `desired` via Remove+Insert of the instance.
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
    /// DecodePixelWidth caps the decode at the ~120px card display size so a
    /// full-res face JPEG isn't decoded for a thumbnail (mirrors
    /// MergeSuggestionVm.ResolveFace).
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
                _cachedAnchorImage = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage
                {
                    DecodePixelWidth = 120,
                    UriSource = new Uri(path),
                };
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
    public bool ActionsEnabled => !_isResolved && !_isBusy;

    private bool _isBusy;
    /// <summary>True while an engine merge/different action is in flight for
    /// this pair; disables both action buttons so a second click can't
    /// double-apply during the up-to-30s engine await.</summary>
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
