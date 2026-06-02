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
    private const int PageSize = 200;
    private static readonly TimeSpan DebounceWindow = TimeSpan.FromMilliseconds(200);

    private readonly ReadStore _store;
    private readonly ClipSearchService _clip;
    private readonly DispatcherQueue _ui;

    private CancellationTokenSource? _searchCts;
    /// <summary>Linked into every async IO call; cancelled by <see cref="Dispose"/>
    /// so a tab-swap-mid-scan unwinds all in-flight work BEFORE the view
    /// disposes the services those calls depend on. Without this the
    /// classic crash was "click Library → click People mid-scan → an
    /// in-flight ClipSearchService.SearchAsync resumes against a disposed
    /// service → ObjectDisposedException escapes to AppDomain.Unhandled".</summary>
    private readonly CancellationTokenSource _disposalCts = new();
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
        // ReadStore.LastOpenError / ClipSearchService.LastSearchError carry
        // humanized open/search failures but raise PropertyChanged off the UI
        // thread (both run with ConfigureAwait(false)). Surface them into
        // ErrorMessage — which StatusText reads — marshaled to the UI thread, so a
        // DB-open or search failure shows the message instead of an indistinguishable
        // empty grid. This also covers the path where OpenAsync throws and the view's
        // Loaded handler skips RefreshAsync (ErrorMessage would otherwise never be set).
        _store.PropertyChanged += OnServiceErrorChanged;
        _clip.PropertyChanged += OnServiceErrorChanged;
        // Per-tile PropertyChanged subscription happens via this hook —
        // FileTile only raises IsSelected on itself, so without forwarding
        // here the VM's SelectedCount stays stale and the bulk-action
        // toolbar visibility breaks.
        Items.CollectionChanged += OnItemsCollectionChanged;
    }

    private void OnServiceErrorChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is not (nameof(ReadStore.LastOpenError) or nameof(ClipSearchService.LastSearchError)))
            return;
        // Prefer the open error (more fundamental) over a transient search error.
        var msg = !string.IsNullOrEmpty(_store.LastOpenError) ? _store.LastOpenError : _clip.LastSearchError;
        if (string.IsNullOrEmpty(msg)) return;
        _ui.TryEnqueue(() => { if (!_disposed) ErrorMessage = msg; });
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        // Cancel disposal CTS FIRST so any in-flight RefreshAsync /
        // SemanticSearchWithSeedAsync running on a thread-pool thread
        // unwinds with OperationCanceledException before the view
        // disposes _clip / _thumbnails out from under it.
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
        // ClipSearchService is owned by the view, disposed there.
    }

    public BatchObservableCollection<FileTile> Items { get; } = new();

    private readonly HashSet<FileTile> _selected = new();
    private IReadOnlyList<FileTile>? _selectedItemsCache;

    // Bulk-selection batching. While depth > 0, per-tile IsSelected toggles
    // update _selected but skip raising PropertyChanged + registry publish.
    // On the final EndBulkSelection, if anything actually changed, fire one
    // batch of notifications. Drops a Ctrl+A on 10K tiles from O(N²) work
    // (N notifications × O(N) SelectedItems.ToList() allocations) to O(N).
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
        // Attach/detach per-tile listeners so we keep _selected in sync.
        // selectionChanged tracks whether _selected membership actually moved,
        // so we re-publish to SelectionRegistry on real changes — the
        // identity-stable merge emits granular Remove/Add (a reorder of a
        // selected tile is Remove then Add of the same instance), and each
        // event arrives here separately; the final event leaves the registry
        // correct.
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
                t.PropertyChanged += OnTilePropertyChanged;
                if (t.IsSelected && _selected.Add(t)) selectionChanged = true;
            }
        }
        if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Reset)
        {
            // ObservableCollection.Clear() doesn't surface OldItems; rebuild
            // from scratch.
            _selected.Clear();
            foreach (var t in Items)
            {
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
        // Snapshot ids once on the UI thread; SelectionRegistry consumers
        // (e.g. DeepAnalyzeView's "Analyze selected" button) read from
        // any thread without re-allocating per read.
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

            IReadOnlyList<FileRow> rows;
            if (string.IsNullOrWhiteSpace(_query))
            {
                rows = await _store.RecentAsync(PageSize, token, _kindFilter == "all" ? null : _kindFilter).ConfigureAwait(false);
            }
            else
            {
                rows = await _clip.SearchAsync(_query, PageSize, token, _kindFilter == "all" ? null : _kindFilter).ConfigureAwait(false);
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

            ApplyOnUi(filtered);
        }
        catch (OperationCanceledException)
        {
            // Outer call superseded OR view unloaded mid-flight — ignore.
        }
        catch (ObjectDisposedException)
        {
            // Services torn down during shutdown — ignore.
        }
        catch (Exception ex)
        {
            if (!_disposed) ErrorMessage = ex.Message;
        }
        finally
        {
            if (!_disposed) IsLoading = false;
        }
    }

    /// <summary>
    /// Replace the grid with files ranked by cosine similarity to a seed
    /// CLIP embedding. Used by the Library tile right-click "Find similar"
    /// action and by the Restructure preview's similar-files lookup.
    /// </summary>
    public async Task SemanticSearchWithSeedAsync(float[] seed, CancellationToken ct)
    {
        if (_disposed) return;
        try
        {
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            IsLoading = true;
            ErrorMessage = null;
            var ranked = await _store.SemanticSearchAsync(seed, PageSize, token, _kindFilter == "all" ? null : _kindFilter).ConfigureAwait(false);
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
            ApplyOnUi(filtered);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex) { if (!_disposed) ErrorMessage = ex.Message; }
        finally { if (!_disposed) IsLoading = false; }
    }

    /// <summary>
    /// Fetch similar files directly using the seed file ID.
    /// Eliminates the multi-step IPC roundtrip.
    /// </summary>
    public async Task FindSimilarAsync(long fileId, CancellationToken ct)
    {
        if (_disposed) return;
        try
        {
            using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
            var token = linked.Token;
            IsLoading = true;
            ErrorMessage = null;
            var similar = await _store.SimilarFilesAsync(fileId, PageSize, token).ConfigureAwait(false);
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
            ApplyOnUi(filtered);
        }
        catch (OperationCanceledException) { /* expected */ }
        catch (ObjectDisposedException) { /* expected during teardown */ }
        catch (Exception ex) { if (!_disposed) ErrorMessage = ex.Message; }
        finally { if (!_disposed) IsLoading = false; }
    }

    private void ApplyOnUi(IReadOnlyList<FileTile> next)
    {
        if (_ui.HasThreadAccess)
        {
            ReplaceItems(next);
        }
        else
        {
            _ui.TryEnqueue(() => ReplaceItems(next));
        }
    }

    private void ReplaceItems(IReadOnlyList<FileTile> next)
    {
        // Identity-stable merge (macOS parity). The old Clear+ReplaceAll(Reset)
        // recreated every FileTile and made ItemsRepeater re-realize every
        // visible element ~1 Hz during a scan — nulling each tile's Thumbnail
        // and racing the async reload, so thumbnails never persisted (the
        // "blank tiles during scan" report). Merging by Id keeps surviving
        // instances (and their loaded Thumbnail) and emits only granular
        // Add/Remove for genuine deltas.
        //
        // Fast path: a fully-disjoint result (e.g. a brand-new search query)
        // has no instances worth preserving, so a single Reset beats
        // remove-all + insert-all granular events. The Reset branch in
        // OnItemsCollectionChanged rebuilds _selected from the new items, so
        // detach old listeners first (Reset doesn't surface OldItems).
        if (Items.Count > 0 && next.Count > 0 && NoOverlapById(Items, next))
        {
            foreach (var t in Items) t.PropertyChanged -= OnTilePropertyChanged;
            Items.ReplaceAll(next);
            return;
        }
        MergeById(Items, next);
    }

    /// <summary>True when no Id in <paramref name="next"/> is already in
    /// <paramref name="items"/> — i.e. the merge would preserve nothing.</summary>
    private static bool NoOverlapById(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        IReadOnlyList<FileTile> next)
    {
        var ids = new HashSet<long>(next.Count);
        foreach (var t in next) ids.Add(t.Id);
        foreach (var t in items) if (ids.Contains(t.Id)) return false;
        return true;
    }

    /// <summary>Reconcile <paramref name="items"/> to match <paramref name="next"/>
    /// by FileTile.Id, in place. Surviving Ids keep their existing instance
    /// (so a loaded Thumbnail survives) and absorb mutable fields via
    /// <see cref="FileTile.MergeMutableFrom"/>; gone Ids are removed; new Ids
    /// inserted at their target index. Reorders use Remove+Insert (never a Move
    /// event — ItemsRepeater handles Move poorly). Static + collection-only so
    /// it's unit-testable without a UI thread.</summary>
    internal static void MergeById(
        System.Collections.ObjectModel.ObservableCollection<FileTile> items,
        IReadOnlyList<FileTile> next)
    {
        if (items.Count == 0)
        {
            foreach (var t in next) items.Add(t);
            return;
        }

        var existingById = new Dictionary<long, FileTile>(items.Count);
        foreach (var t in items) existingById[t.Id] = t;

        // Target sequence: reuse surviving instances (merged), keep new ones.
        var desired = new List<FileTile>(next.Count);
        var nextIds = new HashSet<long>(next.Count);
        foreach (var fresh in next)
        {
            // Defensive: skip a duplicate Id so the same instance can't be
            // inserted twice (a tile bound to two realized elements is a
            // recycle hazard). All current queries return distinct Ids.
            if (!nextIds.Add(fresh.Id)) continue;
            if (existingById.TryGetValue(fresh.Id, out var keep))
            {
                // A different modified-time means a different thumbnail cache
                // key — drop the stale bitmap so it reloads.
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

        // 1) Remove gone rows (backwards so indices stay valid) → Remove events.
        for (int i = items.Count - 1; i >= 0; i--)
        {
            if (!nextIds.Contains(items[i].Id)) items.RemoveAt(i);
        }

        // 2) Align order to `desired` via Remove+Insert of the instance.
        //    items[0..j-1] already equals desired[0..j-1] each iteration.
        for (int j = 0; j < desired.Count; j++)
        {
            var want = desired[j];
            if (j < items.Count && ReferenceEquals(items[j], want)) continue;
            int cur = IndexOfInstance(items, want, j);
            if (cur >= 0) items.RemoveAt(cur);
            items.Insert(j, want);
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
        // BUG-1: two rapid Query setters from the dispatcher could both
        // read the same `prior` and double-dispose. Interlocked.Exchange
        // makes the swap atomic — only one caller ever sees a given old
        // CTS as `prior`.
        var cts = new CancellationTokenSource();
        var prior = Interlocked.Exchange(ref _searchCts, cts);
        if (prior != null)
        {
            try { prior.Cancel(); } catch (ObjectDisposedException) { }
            prior.Dispose();
        }
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(DebounceWindow, cts.Token).ConfigureAwait(false);
                await RefreshAsync(cts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { /* expected */ }
        });
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

    // Fields below are MUTABLE: the engine rewrites them between scan batches
    // (tagging adds tags, Deep Analyze sets ProposedName, OCR/face stages flip
    // HasText/HasFaces). They are change-guarded settable so the identity-stable
    // merge in LibraryViewModel can refresh a SURVIVING tile in place — keeping
    // its already-loaded Thumbnail — instead of replacing the instance and
    // forcing a thumbnail reload. Immutable-per-Id fields (Path/Kind/Size/…)
    // stay init-only above.

    private bool _hasFaces;
    public bool HasFaces
    {
        get => _hasFaces;
        set { if (_hasFaces == value) return; _hasFaces = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasFaces))); }
    }

    private bool _hasText;
    public bool HasText
    {
        get => _hasText;
        set { if (_hasText == value) return; _hasText = value; PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasText))); }
    }

    /// <summary>Top auto-tags from CLIP zero-shot scene tagging + enriched extras.
    /// Library card binds the first 2 via TagChip controls; null/empty
    /// → chip row collapses. Mirrors macOS LibraryView.swift:729-744
    /// `topTags.prefix(2)` behaviour.</summary>
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
    /// <summary>First 2 tags only — Library card shows two chips max,
    /// matching the macOS prefix(2). Cached as a list so the
    /// ItemsControl binding doesn't re-take the slice on every render.</summary>
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

    /// <summary>Deep Analyze's smart-rename proposal. Shown in gold under
    /// the filename when present, matching macOS LibraryView.swift's
    /// proposedName affordance. Null when Deep Analyze hasn't run on
    /// this file (or didn't propose a rename).</summary>
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

    /// <summary>Copy the engine-mutable display fields from a freshly-queried
    /// row onto this surviving instance during the identity-stable merge.
    /// Deliberately does NOT touch IsSelected (selection survives a refresh,
    /// matching macOS) or Thumbnail (the whole point — keep the loaded bitmap).
    /// Each setter is change-guarded, so an unchanged field raises nothing.</summary>
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
                    // Fallback
                }
            }
            return string.Empty;
        }
    }

    /// <summary>Segoe Fluent Icons glyph that summarizes the file's kind
    /// for the top-left badge stack. Mirrors macOS's SF Symbol per-kind
    /// glyph (photo / video / music / doc.text / doc / file). Returns a
    /// neutral file glyph for unknown kinds so the badge never renders
    /// blank space.</summary>
    public string KindBadgeGlyph => Kind switch
    {
        "image" => "", // Picture
        "video" => "", // Video
        "audio" => "", // MusicNote
        "pdf" => "", // PDF
        "doc" => "", // Document
        _ => "", // File
    };

    /// <summary>Human-readable kind label used in the file preview sheet
    /// metadata. Mirrors macOS's kind.capitalized.</summary>
    public string KindDisplay => Kind switch
    {
        "image" => "Image",
        "video" => "Video",
        "audio" => "Audio",
        "pdf" => "PDF",
        "doc" => "Document",
        _ => "File",
    };

    /// <summary>True when the Library card should render a structured kind
    /// chip as the first chip in its row. Suppressed for `other` so files
    /// of unknown kind don't get a meaningless "File" chip.</summary>
    public bool ShowKindChip => Kind != "other";

    /// <summary>True when the card's chip row should be visible at all —
    /// either the kind chip is present or at least one auto-tag landed.
    /// Lets the StackPanel containing both collapse cleanly on bare files
    /// so the caption row keeps its fixed 68 DIP height.</summary>
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
            // silently no-op once the tile is detached. A thumbnail
            // render kicked off by ElementPrepared can complete after
            // ElementClearing already pulled the tile off-screen. Setting
            // Thumbnail on a stale tile would raise PropertyChanged on a
            // dead object whose bindings target a recycled FrameworkElement
            // — harmless in most cases, but a real risk during library
            // refresh races. Detach is set by LibraryView.OnRepeaterElementClearing.
            if (IsDetached) return;
            if (ReferenceEquals(_thumbnail, value)) return;
            _thumbnail = value;
            // A real bitmap landing clears any prior failed-state so the
            // placeholder doesn't linger over a successful retry.
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

    /// <summary>Release the thumbnail bitmap when the tile is recycled out
    /// of the ItemsRepeater virtualization window. Deliberately bypasses the
    /// <see cref="IsDetached"/> guard on the <see cref="Thumbnail"/> setter:
    /// the Image is <c>Source="{x:Bind Thumbnail}"</c>, so without an explicit
    /// null the recycled element keeps the previous file's bitmap bound to
    /// Source and flashes it before the next bind catches up ("rendering from
    /// anything"). Nulling here also lets the BitmapImage be GC'd, bounding
    /// memory on large libraries (off-screen tiles otherwise retain every
    /// bitmap they ever loaded). Mirrors macOS's cancel-and-release on cell
    /// recycle; the ThumbnailService L1 cache makes the reload on re-prepare a
    /// dictionary hit. Must be called BEFORE <see cref="IsDetached"/> flips,
    /// and on the UI thread (raises PropertyChanged for the x:Bind).</summary>
    public void ClearThumbnailForRecycle()
    {
        if (_thumbnail == null) return;
        _thumbnail = null;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasThumbnail)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ShowShimmer)));
    }

    private bool _thumbnailFailed;
    /// <summary>Set by <see cref="Views.Library.LibraryView"/> when the
    /// thumbnail service exhausts its fallback chain and returns null.
    /// Distinguishes "render failed" from "still loading" so the shimmer
    /// can hand off to a broken-image placeholder instead of looping
    /// indefinitely.</summary>
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

    /// <summary>Drives the shimmer overlay's visibility on Library cards.
    /// Visible only while we're still trying to load — collapses both
    /// when a bitmap lands and when the load fails (the placeholder
    /// takes over for the latter).</summary>
    public bool ShowShimmer => _thumbnail == null && !_thumbnailFailed;

    /// <summary>marker the view sets when a tile is cleared
    /// (scrolled out of the ItemsRepeater virtualization window).
    /// Suppresses late thumbnail-render results from binding to a
    /// detached object. Plain field — bound to setter accessed only
    /// from the UI thread, so no synchronization needed.</summary>
    public bool IsDetached { get; set; }

    /// <summary>Modified-at unix seconds, used as part of the thumbnail cache key.</summary>
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

        // Materialise the prefix(2) once so the card binding doesn't
        // allocate on every layout pass.
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

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }
}
