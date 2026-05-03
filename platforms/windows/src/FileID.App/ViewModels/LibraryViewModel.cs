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
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;
using FileID.Services;
using Microsoft.UI.Dispatching;

namespace FileID.ViewModels;

internal sealed class LibraryViewModel : INotifyPropertyChanged
{
    private const int PageSize = 200;
    private static readonly TimeSpan DebounceWindow = TimeSpan.FromMilliseconds(200);

    private readonly ReadStore _store;
    private readonly ClipSearchService _clip;
    private readonly DispatcherQueue _ui;

    private CancellationTokenSource? _searchCts;
    private string _query = string.Empty;
    private string _kindFilter = "all";
    private bool _isLoading;
    private string? _errorMessage;

    public LibraryViewModel(ReadStore store, ClipSearchService clip, DispatcherQueue ui)
    {
        _store = store;
        _clip = clip;
        _ui = ui;
    }

    public ObservableCollection<FileTile> Items { get; } = new();

    public IReadOnlyList<FileTile> SelectedItems
    {
        get
        {
            var list = new List<FileTile>();
            foreach (var t in Items) if (t.IsSelected) list.Add(t);
            return list;
        }
    }

    public int SelectedCount
    {
        get
        {
            int c = 0;
            foreach (var t in Items) if (t.IsSelected) c++;
            return c;
        }
    }

    public void ClearSelection()
    {
        foreach (var t in Items) t.IsSelected = false;
        OnPropertyChanged(nameof(SelectedCount));
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
        try
        {
            IsLoading = true;
            ErrorMessage = null;

            IReadOnlyList<FileRow> rows;
            if (string.IsNullOrWhiteSpace(_query))
            {
                rows = await _store.RecentAsync(PageSize, ct).ConfigureAwait(false);
            }
            else
            {
                rows = await _clip.SearchAsync(_query, PageSize, ct).ConfigureAwait(false);
            }

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
            // Outer call superseded — ignore.
        }
        catch (Exception ex)
        {
            ErrorMessage = ex.Message;
        }
        finally
        {
            IsLoading = false;
        }
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
        var prior = _searchCts;
        var cts = new CancellationTokenSource();
        _searchCts = cts;
        if (prior != null)
        {
            prior.Cancel();
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
    };

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }
}
