// LibraryViewModel — backs the Library tab grid + search bar.

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;
using FileID.Services;
using Microsoft.UI.Dispatching;

namespace FileID.ViewModels;

internal sealed class LibraryViewModel : INotifyPropertyChanged, IDisposable
{
    private const int PageSize = 300;
    private const int RefreshCap = 3000;
    private static readonly TimeSpan DebounceWindow = TimeSpan.FromMilliseconds(200);

    private readonly ReadStore _store;
    private readonly ClipSearchService _clip;
    private readonly DispatcherQueue _ui;

    private CancellationTokenSource? _searchCts;
    /// <summary>Linked into every async IO call; cancelled by Dispose so a tab-swap-mid-scan unwinds all in-flight work BEFORE the view disposes the services those calls depend on.</summary>
    private readonly CancellationTokenSource _disposalCts = new();
    private long _refreshGen;
    private int _activeLoads;
    // Paging for the plain (unsearched) listing: rows loaded so far, whether the database has more, and a guard so only one page loads at a time.
    private int _loadedRows;
    private bool _hasMore;
    private int _loadingMore;
    private volatile string? _listedKind;
    private string _query = string.Empty;
    private string _kindFilter = "all";
    private bool _isLoading;
    private string? _errorMessage;
    private bool _disposed;

    public LibraryViewModel(ReadStore store, ClipSearchService clip, DispatcherQueue ui)
    {
        _store = store;
        _clip = clip;
        _ui = ui;
        // This also covers the path where OpenAsync throws and the view's Loaded handler skips RefreshAsync (ErrorMessage would otherwise never be set).
        _store.PropertyChanged += OnServiceErrorChanged;
        _clip.PropertyChanged += OnServiceErrorChanged;
        // Per-tile PropertyChanged subscription happens via this hook — FileTile only raises IsSelected on itself, so without forwarding here the VM's SelectedCount stays stale and the bulk-action toolbar visibility breaks.
        Items.CollectionChanged += OnItemsCollectionChanged;
    }

    private void OnServiceErrorChanged(object? sender, PropertyChangedEventArgs e)
        => FileID.Services.DebugLog.SafeRun(nameof(OnServiceErrorChanged), () => OnServiceErrorChangedCore(sender, e));

    private void OnServiceErrorChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is not (nameof(ReadStore.LastOpenError) or nameof(ClipSearchService.LastSearchError)))
            return;
        var msg = !string.IsNullOrEmpty(_store.LastOpenError) ? _store.LastOpenError : _clip.LastSearchError;
        if (string.IsNullOrEmpty(msg)) return;
        _ui.TryEnqueue(() => { if (!_disposed) ErrorMessage = msg; });
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        // Cancel disposal CTS FIRST so any in-flight RefreshAsync / SemanticSearchWithSeedAsync running on a thread-pool thread unwinds with OperationCanceledException before the view disposes _clip / _thumbnails out from under it.
        try { _disposalCts.Cancel(); } catch { /* swallow */ }
        try { _searchCts?.Cancel(); } catch { /* swallow */ }
        _searchCts?.Dispose();
        _searchCts = null;
        _store.PropertyChanged -= OnServiceErrorChanged;
        _clip.PropertyChanged -= OnServiceErrorChanged;
        Items.CollectionChanged -= OnItemsCollectionChanged;
        foreach (var t in Items) t.PropertyChanged -= OnTilePropertyChanged;
        _selected.Clear();
        try { _disposalCts.Dispose(); } catch { /* swallow */ }
    }

    public BatchObservableCollection<FileTile> Items { get; } = new();

    private OfflineRoots _offline = OfflineRoots.None;

    internal bool ApplyOfflineRoots(OfflineRoots roots)
    {
        if (_offline.Equals(roots)) return false;
        _offline = roots;
        foreach (var t in Items) t.IsOffline = roots.IsOffline(t.Path);
        return true;
    }

    private readonly HashSet<FileTile> _selected = new();
    private IReadOnlyList<FileTile>? _selectedItemsCache;

    private int _bulkDepth;
    private bool _bulkSelectionDirty;

    public IDisposable BulkSelectionScope()
    {
        _bulkDepth++;
        return new BulkScope(this);
    }

    private void EndBulkSelection()
    {
        if (_bulkDepth == 0) return;
        _bulkDepth--;
        if (_bulkDepth != 0 || !_bulkSelectionDirty) return;
        _bulkSelectionDirty = false;
        _selectedItemsCache = null;
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(SelectedItems));
        PublishSelectionToRegistry();
    }

    private sealed class BulkScope : IDisposable
    {
        private LibraryViewModel? _vm;
        public BulkScope(LibraryViewModel vm) { _vm = vm; }
        public void Dispose()
        {
            var vm = Interlocked.Exchange(ref _vm, null);
            vm?.EndBulkSelection();
        }
    }

    private void OnItemsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        bool selectionChanged = false;
        if (e.OldItems is not null)
        {
            foreach (FileTile t in e.OldItems)
            {
                t.PropertyChanged -= OnTilePropertyChanged;
                if (_selected.Remove(t)) selectionChanged = true;
            }
        }
        if (e.NewItems is not null)
        {
            foreach (FileTile t in e.NewItems)
            {
                t.IsOffline = _offline.IsOffline(t.Path);
                t.PropertyChanged += OnTilePropertyChanged;
                if (t.IsSelected && _selected.Add(t)) selectionChanged = true;
            }
        }
        if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Reset)
        {
            _selected.Clear();
            foreach (var t in Items)
            {
                t.IsOffline = _offline.IsOffline(t.Path);
                t.PropertyChanged -= OnTilePropertyChanged;
                t.PropertyChanged += OnTilePropertyChanged;
                if (t.IsSelected) _selected.Add(t);
            }
            selectionChanged = true;
        }
        if (selectionChanged) _selectedItemsCache = null;
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(SelectedItems));
        if (selectionChanged) PublishSelectionToRegistry();
    }

    private void OnTilePropertyChanged(object? sender, PropertyChangedEventArgs e)
        => FileID.Services.DebugLog.SafeRun(nameof(OnTilePropertyChanged), () => OnTilePropertyChangedCore(sender, e));

    private void OnTilePropertyChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(FileTile.IsSelected)) return;
        if (sender is not FileTile t) return;
        bool changed = t.IsSelected ? _selected.Add(t) : _selected.Remove(t);
        if (!changed) return;
        _selectedItemsCache = null;
        if (_bulkDepth > 0)
        {
            _bulkSelectionDirty = true;
            return;
        }
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(SelectedItems));
        PublishSelectionToRegistry();
    }

    private void PublishSelectionToRegistry()
    {
        // Snapshot ids once on the UI thread; SelectionRegistry consumers (e.g.
        var ids = new long[_selected.Count];
        int i = 0;
        foreach (var t in _selected) ids[i++] = t.Id;
        FileID.Services.SelectionRegistry.Instance.LibrarySelection = ids;
    }

    public IReadOnlyList<FileTile> SelectedItems => _selectedItemsCache ??= _selected.ToList();

    public int SelectedCount => _selected.Count;

    public void ClearSelection()
    {
        if (_selected.Count == 0) return;
        var snapshot = _selected.ToList();
        using (BulkSelectionScope())
        {
            foreach (var t in snapshot) t.IsSelected = false;
        }
    }

    public string Query
    {
        get => _query;
        set
        {
            if (_query == value) return;
            _query = value;
            _showWeaker = false;
            OnPropertyChanged();
            ScheduleRefresh();
        }
    }

    public string KindFilter
    {
        get => _kindFilter;
        set
        {
            if (_kindFilter == value) return;
            _kindFilter = value;
            OnPropertyChanged();
            ScheduleRefresh();
        }
    }

    private string? _searchSummary;
    private int _hiddenWeaker;
    private bool _showWeaker;

    public int HiddenWeaker
    {
        get => _hiddenWeaker;
        private set
        {
            if (_hiddenWeaker == value) return;
            _hiddenWeaker = value;
            OnPropertyChanged();
        }
    }

    public void ShowWeakerMatches()
    {
        _showWeaker = true;
        ScheduleRefresh();
    }

    public string? SearchSummary
    {
        get => _searchSummary;
        private set
        {
            if (_searchSummary == value) return;
            _searchSummary = value;
            OnPropertyChanged();
        }
    }

    private int? _totalCount;

    public int? TotalCount
    {
        get => _totalCount;
        private set
        {
            if (_totalCount == value) return;
            _totalCount = value;
            OnPropertyChanged();
        }
    }

    internal static int CountForKind(IReadOnlyDictionary<string, int> counts, string? kind)
    {
        if (kind is not null) return counts.TryGetValue(kind, out var n) ? n : 0;
        int total = 0;
        foreach (var n in counts.Values) total += n;
        return total;
    }

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

    public Task RefreshAsync(CancellationToken ct) => RefreshAsync(liveScan: false, ct);

    /// <summary>liveScan marks the once-a-second refresh while a scan is adding files: it re-reads only the first page, puts the new files in front of what is already listed, and reuses recent per-kind counts.</summary>
    public async Task RefreshAsync(bool liveScan, CancellationToken ct)
    {
        if (_disposed) return;
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            // Linked token created inside the try: a Dispose() race after the _disposed check makes _disposalCts.Token throw ObjectDisposedException, caught below as a clean teardown no-op instead of escaping to the caller.
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() =>
            {
                IsLoading = true;
                ErrorMessage = null;
            });

            IReadOnlyList<FileRow> rows;
            int? total = null;
            string? summary = null;
            int hiddenWeaker = 0;
            bool pageable = false;
            bool headOnly = false;
            string? listedKind = null;
            if (string.IsNullOrWhiteSpace(_query))
            {
                // Microsoft.Data.Sqlite is fake-async (the query runs inline on whatever thread acquires the gate).
                listedKind = _kindFilter;
                headOnly = liveScan && _listedKind == listedKind && Volatile.Read(ref _loadedRows) > 0;
                int want = headOnly ? PageSize : RefreshWindow(_listedKind == listedKind, Volatile.Read(ref _loadedRows));
                string? kind = listedKind == "all" ? null : listedKind;
                var listing = await Task.Run(async () =>
                {
                    var recent = await _store.RecentAsync(want, token, kind).ConfigureAwait(false);
                    var counts = await KindCountsAsync(liveScan, token).ConfigureAwait(false);
                    return (Rows: recent, Total: CountForKind(counts, kind));
                }, token).ConfigureAwait(false);
                rows = listing.Rows;
                total = listing.Total;
                pageable = rows.Count == want;
            }
            else
            {
                rows = await _clip.SearchAsync(_query, PageSize, token, _kindFilter == "all" ? null : _kindFilter, text => summary = text, _showWeaker, count => hiddenWeaker = count).ConfigureAwait(false);
            }

            if (_disposed || token.IsCancellationRequested) return;
            var filtered = new List<FileTile>(rows.Count);
            foreach (var r in rows)
            {
                if (_kindFilter != "all" && !string.Equals(r.Kind, _kindFilter, StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }
                filtered.Add(FileTile.From(r));
            }

            int loaded = rows.Count;
            if (headOnly)
            {
                ApplyHeadOnUi(filtered, myGen, total, listedKind);
            }
            else
            {
                ApplyOnUi(filtered, myGen, pageable, loaded, total, listedKind, summary, hiddenWeaker);
            }
        }
        catch (OperationCanceledException)
        {
        }
        catch (ObjectDisposedException)
        {
        }
        catch (Exception ex)
        {
            OnUi(() => { if (!_disposed && Interlocked.Read(ref _refreshGen) == myGen) ErrorMessage = ex.Message; });
        }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

    public async Task SemanticSearchWithSeedAsync(float[] seed, CancellationToken ct)
    {
        if (_disposed) return;
        CancelPendingSearchDebounce();
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() =>
            {
                IsLoading = true;
                ErrorMessage = null;
            });
            // Full clip_embeddings table scan + dot-product loop — and this VM is reached from an async-void context-menu handler on the UI thread, where Microsoft.Data.Sqlite's fake-async runs the scan inline.
            var ranked = await Task.Run(() => _store.SemanticSearchAsync(seed, PageSize, token, _kindFilter == "all" ? null : _kindFilter), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            var filtered = new List<FileTile>(ranked.Count);
            foreach (var hit in ranked)
            {
                if (_kindFilter != "all" && !string.Equals(hit.Row.Kind, _kindFilter, StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }
                filtered.Add(FileTile.From(hit.Row));
            }
            ApplyOnUi(filtered, myGen);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex) { OnUi(() => { if (!_disposed && Interlocked.Read(ref _refreshGen) == myGen) ErrorMessage = ex.Message; }); }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

    public async Task FindSimilarAsync(long fileId, CancellationToken ct)
    {
        if (_disposed) return;
        CancelPendingSearchDebounce();
        long myGen = Interlocked.Increment(ref _refreshGen);
        Interlocked.Increment(ref _activeLoads);
        try
        {
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            OnUi(() =>
            {
                IsLoading = true;
                ErrorMessage = null;
            });
            // Reached from an async-void context-menu handler on the UI thread; Microsoft.Data.Sqlite's fake-async would run the whole scan inline.
            var similar = await Task.Run(async () =>
            {
                var request = EngineSimilarity.SeedRequest(fileId, _store.Scope?.Root.Id, EngineSimilarity.MaxResults);
                if (await _clip.EngineRankedIdsAsync(request, token).ConfigureAwait(false) is { } ids)
                {
                    var ranked = await _store.RowsByIdsAsync(ids, token).ConfigureAwait(false);
                    if (ranked.Count > 0) return ranked;
                }
                return await _store.SimilarFilesAsync(fileId, PageSize, token).ConfigureAwait(false);
            }, token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            var filtered = new List<FileTile>(similar.Count);
            foreach (var r in similar)
            {
                if (_kindFilter != "all" && !string.Equals(r.Kind, _kindFilter, StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }
                filtered.Add(FileTile.From(r));
            }
            ApplyOnUi(filtered, myGen);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex) { OnUi(() => { if (!_disposed && Interlocked.Read(ref _refreshGen) == myGen) ErrorMessage = ex.Message; }); }
        finally
        {
            Interlocked.Decrement(ref _activeLoads);
            OnUi(() => { if (!_disposed) IsLoading = Volatile.Read(ref _activeLoads) > 0; });
        }
    }

    internal static int RefreshWindow(bool sameListing, int loadedRows) =>
        sameListing ? Math.Min(Math.Max(PageSize, loadedRows), RefreshCap) : PageSize;

    private void ApplyOnUi(IReadOnlyList<FileTile> next, long gen, bool hasMore = false, int loadedRows = 0, int? total = null, string? listedKind = null, string? summary = null, int hiddenWeaker = 0)
    {
        // Drop results from a refresh a newer one has already superseded — checked on the UI thread right before the swap so it also catches a refresh that started during the dispatch gap.
        void Apply()
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            SearchSummary = summary;
            HiddenWeaker = hiddenWeaker;
            ReplaceItems(next);
            TotalCount = total;
            _listedKind = listedKind;
            _hasMore = hasMore;
            Volatile.Write(ref _loadedRows, hasMore || loadedRows > 0 ? loadedRows : 0);
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

    private void ApplyHeadOnUi(IReadOnlyList<FileTile> head, long gen, int? total, string? listedKind)
    {
        void Apply()
        {
            if (Interlocked.Read(ref _refreshGen) != gen) return;
            int added = _listedKind == listedKind ? MergeHeadPage(Items, head) : -1;
            if (added < 0)
            {
                ReplaceItems(head);
                _listedKind = listedKind;
                _hasMore = head.Count == PageSize;
                Volatile.Write(ref _loadedRows, head.Count);
            }
            else
            {
                Volatile.Write(ref _loadedRows, Volatile.Read(ref _loadedRows) + added);
            }
            TotalCount = total;
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

    internal static int MergeHeadPage(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        IReadOnlyList<FileTile> head)
    {
        if (items.Count == 0 || head.Count == 0) return -1;
        var existingById = new Dictionary<long, FileTile>(items.Count);
        foreach (var t in items) existingById[t.Id] = t;

        var seen = new HashSet<long>(head.Count);
        var fresh = new List<FileTile>();
        var known = new List<FileTile>(head.Count);
        bool freshAfterKnown = false;
        foreach (var t in head)
        {
            if (!seen.Add(t.Id)) continue;
            if (existingById.TryGetValue(t.Id, out var keep))
            {
                if (!NullableDoubleEquals(keep.ModifiedAt, t.ModifiedAt)) keep.ClearThumbnailForRecycle();
                keep.MergeMutableFrom(t);
                known.Add(keep);
            }
            else
            {
                if (known.Count > 0) freshAfterKnown = true;
                fresh.Add(t);
            }
        }
        if (known.Count == 0) return -1;

        bool sameOrder = !freshAfterKnown && known.Count <= items.Count;
        for (int i = 0; sameOrder && i < known.Count; i++)
        {
            sameOrder = ReferenceEquals(items[i], known[i]);
        }
        if (sameOrder)
        {
            if (fresh.Count == 0) return 0;
            if (items is BatchObservableCollection<FileTile> batch)
            {
                batch.PrependRange(fresh);
            }
            else
            {
                for (int i = 0; i < fresh.Count; i++) items.Insert(i, fresh[i]);
            }
            return fresh.Count;
        }

        var desired = new List<FileTile>(items.Count + fresh.Count);
        foreach (var t in head)
        {
            if (!seen.Remove(t.Id)) continue;
            desired.Add(existingById.TryGetValue(t.Id, out var keep) ? keep : t);
        }
        var inHead = new HashSet<long>(desired.Count);
        foreach (var t in desired) inHead.Add(t.Id);
        foreach (var t in items)
        {
            if (inHead.Add(t.Id)) desired.Add(t);
        }
        MergeById(items, desired);
        return fresh.Count;
    }

    private readonly object _kindCountsLock = new();
    private IReadOnlyDictionary<string, int>? _kindCounts;
    private LibraryScope? _kindCountsScope;
    private long _kindCountsAtTick;
    private const long KindCountsLiveReuseMs = 5000;

    private async Task<IReadOnlyDictionary<string, int>> KindCountsAsync(bool liveScan, CancellationToken ct)
    {
        var scope = _store.Scope;
        if (liveScan)
        {
            lock (_kindCountsLock)
            {
                if (_kindCounts is { } cached
                    && ReferenceEquals(_kindCountsScope, scope)
                    && Environment.TickCount64 - _kindCountsAtTick < KindCountsLiveReuseMs)
                {
                    return cached;
                }
            }
        }
        var counts = await _store.KindCountsAsync(ct).ConfigureAwait(false);
        lock (_kindCountsLock)
        {
            _kindCounts = counts;
            _kindCountsScope = scope;
            _kindCountsAtTick = Environment.TickCount64;
        }
        return counts;
    }

    public async Task LoadMoreAsync()
    {
        if (_disposed || !_hasMore || !string.IsNullOrWhiteSpace(_query)) return;
        if (Interlocked.Exchange(ref _loadingMore, 1) == 1) return;
        long gen = Interlocked.Read(ref _refreshGen);
        int offset = Volatile.Read(ref _loadedRows);
        string? kind = _kindFilter == "all" ? null : _kindFilter;
        try
        {
            var token = _disposalCts.Token;
            var rows = await Task.Run(() => _store.RecentAsync(PageSize, token, kind, offset), token).ConfigureAwait(false);
            if (_disposed || token.IsCancellationRequested) return;
            var page = new List<FileTile>(rows.Count);
            foreach (var r in rows) page.Add(FileTile.From(r));
            OnUi(() =>
            {
                if (Interlocked.Read(ref _refreshGen) != gen || Volatile.Read(ref _loadedRows) != offset) return;
                Items.AppendRange(WithoutKnownIds(Items, page));
                Volatile.Write(ref _loadedRows, offset + rows.Count);
                _hasMore = rows.Count == PageSize;
            });
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
        finally
        {
            Volatile.Write(ref _loadingMore, 0);
        }
    }

    /// <summary>Run a UI-affined mutation on the captured dispatcher.</summary>
    private void OnUi(Action action)
    {
        if (_ui.HasThreadAccess) action();
        else _ui.TryEnqueue(() => { if (!_disposed) action(); });
    }

    private void ReplaceItems(IReadOnlyList<FileTile> next)
    {
        // The old Clear+ReplaceAll(Reset) recreated every FileTile and made ItemsRepeater re-realize every visible element ~1 Hz during a scan — nulling each tile's Thumbnail and racing the async reload, so thumbnails never persisted (the "blank tiles during scan" report).
        if (Items.Count > 0 && next.Count > 0 && NoOverlapById(Items, next))
        {
            foreach (var t in Items) t.PropertyChanged -= OnTilePropertyChanged;
            Items.ReplaceAll(next);
            return;
        }
        MergeById(Items, next);
    }

    /// <summary>The tiles of page whose Id is not already in items (or earlier in the page), in page order.</summary>
    internal static List<FileTile> WithoutKnownIds(
        IReadOnlyCollection<FileTile> items,
        IReadOnlyList<FileTile> page)
    {
        var known = new HashSet<long>(items.Count + page.Count);
        foreach (var t in items) known.Add(t.Id);
        var fresh = new List<FileTile>(page.Count);
        foreach (var t in page)
        {
            if (known.Add(t.Id)) fresh.Add(t);
        }
        return fresh;
    }

    private static bool NoOverlapById(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        IReadOnlyList<FileTile> next)
    {
        var ids = new HashSet<long>(next.Count);
        foreach (var t in next) ids.Add(t.Id);
        foreach (var t in items) if (ids.Contains(t.Id)) return false;
        return true;
    }

    /// <summary>Reorders use Remove+Insert (never a Move event — ItemsRepeater handles Move poorly).</summary>
    internal static void MergeById(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        IReadOnlyList<FileTile> next)
    {
        if (items.Count == 0)
        {
            var fresh = WithoutKnownIds(items, next);
            if (items is BatchObservableCollection<FileTile> batch)
            {
                batch.AppendRange(fresh);
            }
            else
            {
                foreach (var t in fresh) items.Add(t);
            }
            return;
        }

        var existingById = new Dictionary<long, FileTile>(items.Count);
        foreach (var t in items) existingById[t.Id] = t;

        var desired = new List<FileTile>(next.Count);
        var nextIds = new HashSet<long>(next.Count);
        foreach (var fresh in next)
        {
            // Defensive: skip a duplicate Id so the same instance can't be inserted twice (a tile bound to two realized elements is a recycle hazard).
            if (!nextIds.Add(fresh.Id)) continue;
            if (existingById.TryGetValue(fresh.Id, out var keep))
            {
                // A different modified-time means a different thumbnail cache key — drop the stale bitmap so it reloads.
                if (!NullableDoubleEquals(keep.ModifiedAt, fresh.ModifiedAt))
                {
                    keep.ClearThumbnailForRecycle();
                }
                keep.MergeMutableFrom(fresh);
                desired.Add(keep);
            }
            else
            {
                desired.Add(fresh);
            }
        }

        for (int i = items.Count - 1; i >= 0; i--)
        {
            if (!nextIds.Contains(items[i].Id)) items.RemoveAt(i);
        }

        for (int j = 0; j < desired.Count; j++)
        {
            var want = desired[j];
            if (j < items.Count && ReferenceEquals(items[j], want)) continue;
            int cur = IndexOfInstance(items, want, j);
            if (cur >= 0) items.RemoveAt(cur);
            items.Insert(j, want);
        }

        // 3) Anything left past the desired sequence is a stale duplicate instance of an Id already placed above.
        for (int i = items.Count - 1; i >= desired.Count; i--)
        {
            items.RemoveAt(i);
        }
    }

    private static int IndexOfInstance(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        FileTile want,
        int startAt)
    {
        for (int i = startAt; i < items.Count; i++)
        {
            if (ReferenceEquals(items[i], want)) return i;
        }
        return -1;
    }

    private static bool NullableDoubleEquals(double? a, double? b)
        => a.HasValue == b.HasValue && (!a.HasValue || a.Value.Equals(b!.Value));

    private void ScheduleRefresh()
    {
        // Interlocked.Exchange makes the swap atomic — only one caller ever sees a given old CTS as `prior`.
        var cts = new CancellationTokenSource();
        var prior = Interlocked.Exchange(ref _searchCts, cts);
        if (prior != null)
        {
            try { prior.Cancel(); } catch (ObjectDisposedException) { }
            prior.Dispose();
        }
        // Snapshot the token while `cts` is guaranteed alive.
        var token = cts.Token;
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(DebounceWindow, token).ConfigureAwait(false);
                await RefreshAsync(token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { /* expected */ }
            catch (ObjectDisposedException) { /* cts disposed by a newer refresh */ }
        });
    }

    /// <summary>Drop a queued search debounce so an explicit context-menu action (Find similar / semantic search) isn't undone when the still-pending keystroke debounce fires AFTER it. _refreshGen orders by start-of-execution and the 200 ms debounce makes an earlier keystroke's refresh run (and thus win) last, so the explicit action must cancel the queued refresh outright.</summary>
    private void CancelPendingSearchDebounce()
    {
        var prior = Interlocked.Exchange(ref _searchCts, null);
        if (prior != null)
        {
            try { prior.Cancel(); } catch (ObjectDisposedException) { }
            prior.Dispose();
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name ?? string.Empty));
}

internal sealed class FileTile : INotifyPropertyChanged
{
    public required long Id { get; init; }
    public required string Path { get; init; }
    public required string FileName { get; init; }
    public required string Kind { get; init; }
    public required long SizeBytes { get; init; }

    // They are change-guarded settable so the identity-stable merge in LibraryViewModel can refresh a SURVIVING tile in place — keeping its already-loaded Thumbnail — instead of replacing the instance and forcing a thumbnail reload.

    private bool _hasFaces;
    public bool HasFaces
    {
        get => _hasFaces;
        set
        {
            if (_hasFaces == value) return;
            _hasFaces = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasFaces)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(AccessibleName)));
        }
    }

    private bool _hasText;
    public bool HasText
    {
        get => _hasText;
        set
        {
            if (_hasText == value) return;
            _hasText = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasText)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(AccessibleName)));
        }
    }

    private System.Collections.Generic.IReadOnlyList<string> _tags = System.Array.Empty<string>();
    public System.Collections.Generic.IReadOnlyList<string> Tags
    {
        get => _tags;
        set
        {
            if (ReferenceEquals(_tags, value) || _tags.SequenceEqual(value)) return;
            _tags = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Tags)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasTags)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasChips)));
        }
    }
    public bool HasTags => Tags is { Count: > 0 };
    /// <summary>First 2 tags only — Library card shows two chips max, matching the macOS prefix(2).</summary>
    private System.Collections.Generic.IReadOnlyList<string> _topTwoTags = System.Array.Empty<string>();
    public System.Collections.Generic.IReadOnlyList<string> TopTwoTags
    {
        get => _topTwoTags;
        set
        {
            if (ReferenceEquals(_topTwoTags, value) || _topTwoTags.SequenceEqual(value)) return;
            _topTwoTags = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(TopTwoTags)));
        }
    }

    private string? _proposedName;
    public string? ProposedName
    {
        get => _proposedName;
        set
        {
            if (_proposedName == value) return;
            _proposedName = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ProposedName)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasProposedName)));
        }
    }
    public bool HasProposedName => !string.IsNullOrWhiteSpace(ProposedName);

    private bool _isOffline;
    public bool IsOffline
    {
        get => _isOffline;
        set
        {
            if (_isOffline == value) return;
            _isOffline = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsOffline)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(AccessibleName)));
        }
    }

    /// <summary>Each setter is change-guarded, so an unchanged field raises nothing.</summary>
    public void MergeMutableFrom(FileTile fresh)
    {
        Tags = fresh.Tags;
        TopTwoTags = fresh.TopTwoTags;
        ProposedName = fresh.ProposedName;
        HasFaces = fresh.HasFaces;
        HasText = fresh.HasText;
    }

    public string SizeDisplay => FormatSize(SizeBytes);

    public string DateDisplay
    {
        get
        {
            if (ModifiedAt.HasValue)
            {
                try
                {
                    var dt = System.DateTimeOffset.FromUnixTimeSeconds((long)ModifiedAt.Value).LocalDateTime;
                    return dt.ToString("d");
                }
                catch
                {
                }
            }
            return string.Empty;
        }
    }

    public string AccessibleName
    {
        get
        {
            var parts = new List<string> { FileName, KindDisplay };
            if (IsOffline) parts.Add("offline");
            if (DateDisplay.Length > 0) parts.Add(DateDisplay);
            if (HasFaces) parts.Add("contains faces");
            if (HasText) parts.Add("contains text");
            return string.Join(", ", parts);
        }
    }

    /// <summary>Returns a neutral file glyph for unknown kinds so the badge never renders blank space.</summary>
    public string KindBadgeGlyph => Kind switch
    {
        "image" => "", // Picture
        "video" => "", // Video
        "audio" => "", // MusicNote
        "pdf" => "", // PDF
        "doc" => "", // Document
        _ => "", // File
    };

    public string KindDisplay => Kind switch
    {
        "image" => "Image",
        "video" => "Video",
        "audio" => "Audio",
        "pdf" => "PDF",
        "doc" => "Document",
        _ => "File",
    };

    public bool ShowKindChip => Kind != "other";

    public bool HasChips => ShowKindChip || HasTags;

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

    private Microsoft.UI.Xaml.Media.Imaging.BitmapImage? _thumbnail;
    public Microsoft.UI.Xaml.Media.Imaging.BitmapImage? Thumbnail
    {
        get => _thumbnail;
        set
        {
            // Setting Thumbnail on a stale tile would raise PropertyChanged on a dead object whose bindings target a recycled FrameworkElement — harmless in most cases, but a real risk during library refresh races.
            if (IsDetached) return;
            if (ReferenceEquals(_thumbnail, value)) return;
            _thumbnail = value;
            // A real bitmap landing clears any prior failed-state so the placeholder doesn't linger over a successful retry.
            if (value != null && _thumbnailFailed)
            {
                _thumbnailFailed = false;
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ThumbnailFailed)));
            }
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasThumbnail)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowShimmer)));
        }
    }

    public bool HasThumbnail => _thumbnail != null;

    /// <summary>Release the thumbnail bitmap when the tile is recycled out of the ItemsRepeater virtualization window.</summary>
    public void ClearThumbnailForRecycle()
    {
        if (_thumbnail == null) return;
        _thumbnail = null;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasThumbnail)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowShimmer)));
    }

    private bool _thumbnailFailed;
    /// <summary>Distinguishes "render failed" from "still loading" so the shimmer can hand off to a broken-image placeholder instead of looping indefinitely.</summary>
    public bool ThumbnailFailed
    {
        get => _thumbnailFailed;
        set
        {
            if (IsDetached) return;
            if (_thumbnailFailed == value) return;
            _thumbnailFailed = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ThumbnailFailed)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowShimmer)));
        }
    }

    /// <summary>Visible only while we're still trying to load — collapses both when a bitmap lands and when the load fails (the placeholder takes over for the latter).</summary>
    public bool ShowShimmer => _thumbnail == null && !_thumbnailFailed;

    /// <summary>Plain field — bound to setter accessed only from the UI thread, so no synchronization needed.</summary>
    public bool IsDetached { get; set; }

    public double? ModifiedAt { get; init; }

    public event PropertyChangedEventHandler? PropertyChanged;

    public static FileTile From(FileRow r)
    {
        var rawTags = r.Tags ?? (System.Collections.Generic.IReadOnlyList<string>)System.Array.Empty<string>();
        var formattedTags = new System.Collections.Generic.List<string>(rawTags.Count);
        foreach (var t in rawTags)
        {
            formattedTags.Add(FileID.Theme.Controls.TagChip.FormatTag(t));
        }
        var tags = (System.Collections.Generic.IReadOnlyList<string>)formattedTags;

        // Materialise the prefix(2) once so the card binding doesn't allocate on every layout pass.
        var topTwo = tags.Count <= 2
            ? tags
            : (System.Collections.Generic.IReadOnlyList<string>)new System.Collections.Generic.List<string>
            {
                tags[0],
                tags[1],
            };
        return new FileTile
        {
            Id = r.Id,
            Path = r.Path,
            FileName = System.IO.Path.GetFileName(r.Path),
            Kind = r.Kind,
            SizeBytes = r.SizeBytes,
            HasFaces = r.HasFaces,
            HasText = r.HasText,
            ModifiedAt = r.ModifiedAt,
            Tags = tags,
            TopTwoTags = topTwo,
            ProposedName = r.ProposedName,
        };
    }

    private static string FormatSize(long bytes) => FileID.Services.ByteFormat.File(bytes);
}
