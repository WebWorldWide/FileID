// LibraryViewModel — backs the Library tab grid + search bar.
//
// Mirror of macOS app/Sources/FileID/Library/LibraryViewModel.swift. The
// shape is the same: a debounced query string, a kind filter, a page of
// FileTile items, plus a banner state machine. The Windows port runs on
// INotifyPropertyChanged + DispatcherQueue marshalling instead of
// SwiftUI's @Observable.

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
        // V14.9-B1: maintain a HashSet of selected tiles in O(1) per
        // selection change instead of re-walking every Item on each
        // SelectedCount/SelectedItems read. Subscribe to PropertyChanged
        // on every tile via the CollectionChanged hook; without this the
        // VM's SelectedCount binding was actually never re-fired on
        // per-tile toggle (PropertyChanged on FileTile only raised
        // IsSelected on itself), so the bulk-action toolbar visibility
        // was silently stale.
        Items.CollectionChanged += OnItemsCollectionChanged;
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
        // V14.9-B1: detach the per-tile listeners we attached in
        // OnItemsCollectionChanged so the VM can be GC'd cleanly.
        Items.CollectionChanged -= OnItemsCollectionChanged;
        foreach (var t in Items) t.PropertyChanged -= OnTilePropertyChanged;
        _selected.Clear();
        try { _disposalCts.Dispose(); } catch { /* swallow */ }
        // ClipSearchService is owned by the view, disposed there.
    }

    public ObservableCollection<FileTile> Items { get; } = new();

    private readonly HashSet<FileTile> _selected = new();

    private void OnItemsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        // Attach/detach per-tile listeners so we keep _selected in sync.
        if (e.OldItems is not null)
        {
            foreach (FileTile t in e.OldItems)
            {
                t.PropertyChanged -= OnTilePropertyChanged;
                if (_selected.Remove(t))
                {
                    // implicit deselect on removal; defer the notify until
                    // after the full collection change so a Reset doesn't
                    // raise N events.
                }
            }
        }
        if (e.NewItems is not null)
        {
            foreach (FileTile t in e.NewItems)
            {
                t.PropertyChanged += OnTilePropertyChanged;
                if (t.IsSelected) _selected.Add(t);
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
        }
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(SelectedItems));
    }

    private void OnTilePropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(FileTile.IsSelected)) return;
        if (sender is not FileTile t) return;
        bool changed = t.IsSelected ? _selected.Add(t) : _selected.Remove(t);
        if (changed)
        {
            OnPropertyChanged(nameof(SelectedCount));
            OnPropertyChanged(nameof(SelectedItems));
        }
    }

    public IReadOnlyList<FileTile> SelectedItems => _selected.ToList();

    public int SelectedCount => _selected.Count;

    public void ClearSelection()
    {
        if (_selected.Count == 0) return;
        // Snapshot before mutating so the per-tile callback doesn't
        // remove from a collection we're iterating.
        var snapshot = _selected.ToList();
        foreach (var t in snapshot) t.IsSelected = false;
        // The per-tile callback already raised PropertyChanged for each
        // toggle; raise once more in case any tile silently failed to
        // notify (defensive — shouldn't happen).
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(SelectedItems));
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
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
        var token = linked.Token;
        try
        {
            IsLoading = true;
            ErrorMessage = null;

            IReadOnlyList<FileRow> rows;
            if (string.IsNullOrWhiteSpace(_query))
            {
                rows = await _store.RecentAsync(PageSize, token).ConfigureAwait(false);
            }
            else
            {
                rows = await _clip.SearchAsync(_query, PageSize, token).ConfigureAwait(false);
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
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, _disposalCts.Token);
        var token = linked.Token;
        try
        {
            IsLoading = true;
            ErrorMessage = null;
            var ranked = await _store.SemanticSearchAsync(seed, PageSize, token).ConfigureAwait(false);
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
        Items.Clear();
        foreach (var t in next)
        {
            Items.Add(t);
        }
    }

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
    public required bool HasFaces { get; init; }
    public required bool HasText { get; init; }

    public string SizeDisplay => FormatSize(SizeBytes);

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
            if (ReferenceEquals(_thumbnail, value)) return;
            _thumbnail = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Thumbnail)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasThumbnail)));
        }
    }

    public bool HasThumbnail => _thumbnail != null;

    /// <summary>Modified-at unix seconds, used as part of the thumbnail cache key.</summary>
    public double? ModifiedAt { get; init; }

    public event PropertyChangedEventHandler? PropertyChanged;

    public static FileTile From(FileRow r) => new()
    {
        Id = r.Id,
        Path = r.Path,
        FileName = System.IO.Path.GetFileName(r.Path),
        Kind = r.Kind,
        SizeBytes = r.SizeBytes,
        HasFaces = r.HasFaces,
        HasText = r.HasText,
        ModifiedAt = r.ModifiedAt,
    };

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }
}
