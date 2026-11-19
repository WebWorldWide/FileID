// ClipSearchService — orchestrates CLIP semantic search.
// FTS5 fallback returns matching results when the CLIP embedding path isn't available; otherwise the engine reply lights up real semantic search via the IPC round-trip.

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.ViewModels;

namespace FileID.Services;

internal sealed class ClipSearchService : IDisposable, INotifyPropertyChanged
{
    private readonly ReadStore _store;
    private readonly ConcurrentDictionary<string, TaskCompletionSource<float[]?>> _inflight = new();
    private bool _disposed;

    private readonly QueryEmbeddingCache _queryCache = new(capacity: 24);

    private int _generation;

    private string? _lastSearchError;
    public string? LastSearchError
    {
        get => _lastSearchError;
        private set
        {
            if (_lastSearchError == value) return;
            _lastSearchError = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LastSearchError)));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    public ClipSearchService(ReadStore store)
    {
        _store = store;
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        EngineClient.Instance.PropertyChanged -= OnEngineClientChanged;
        foreach (var kv in _inflight)
        {
            kv.Value.TrySetResult(null);
        }
        _inflight.Clear();
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ClipSearchService.OnEngineClientChanged", () =>
        {
            // This handler can fire after Dispose() — Dispose() detaches the subscription, but a notification already in flight on another thread (tab-nav disposal racing an engine state change) can land here with _disposed already true.
            if (_disposed) return;
            if (e.PropertyName == nameof(EngineClient.State))
            {
                DebugLog.Info($"[ENGINE-SUB:ClipSearchService] {e.PropertyName}");
                var newState = EngineClient.Instance.State;
                if (newState != EngineClient.LifecycleState.Ready)
                {
                    Interlocked.Increment(ref _generation);
                    if (!_inflight.IsEmpty)
                    {
                        DebugLog.Info($"[CLIP] engine state={newState}; faulting {_inflight.Count} pending search(es).");
                        foreach (var kv in _inflight)
                        {
                            kv.Value.TrySetException(
                                new InvalidOperationException("Engine respawned mid-query."));
                        }
                        _inflight.Clear();
                    }
                }
                return;
            }
            if (e.PropertyName != nameof(EngineClient.LastClipTextEmbedding)) return;
            DebugLog.Info($"[ENGINE-SUB:ClipSearchService] {e.PropertyName}");
            var emb = EngineClient.Instance.LastClipTextEmbedding;
            if (emb is null) return;
            if (_inflight.TryRemove(emb.QueryId, out var tcs))
            {
                tcs.TrySetResult(emb.Embedding?.ToArray());
            }
        });

    public async Task<float[]?> EmbedQueryAsync(string query, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(query))
        {
            return null;
        }
        var cacheKey = query.Trim();
        if (_queryCache.TryGet(cacheKey, out var cached))
        {
            LastSearchError = null;
            return cached;
        }
        var queryId = Guid.NewGuid().ToString("N");
        var tcs = new TaskCompletionSource<float[]?>(TaskCreationOptions.RunContinuationsAsynchronously);
        _inflight[queryId] = tcs;
        try
        {
            await EngineClient.Instance.EmbedTextQueryAsync(query, queryId).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            _inflight.TryRemove(queryId, out _);
            throw; // user-initiated; let it propagate
        }
        catch (Exception ex)
        {
            _inflight.TryRemove(queryId, out _);
            DebugLog.Warn($"[CLIP] EmbedTextQueryAsync threw: {ex.Message}");
            LastSearchError = "Search unavailable: " + ex.Message;
            return null;
        }

        using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        cts.CancelAfter(TimeSpan.FromSeconds(5));
        await using var reg = cts.Token.Register(() =>
        {
            if (_inflight.TryRemove(queryId, out var t))
            {
                t.TrySetResult(null);
            }
        });
        try
        {
            var result = await tcs.Task.ConfigureAwait(false);
            if (result is { Length: 0 }) { LastSearchError = null; return null; }
            if (result is not null)
            {
                LastSearchError = null;
                _queryCache.Store(cacheKey, result);
            }
            return result;
        }
        catch (OperationCanceledException)
        {
            throw; // user-initiated cancellation
        }
        catch (InvalidOperationException ex) when (ex.Message.Contains("respawned", StringComparison.Ordinal))
        {
            // Surface a clear banner; the caller (LibraryView search) treats null as "fall through to FTS5" so the user still gets results.
            DebugLog.Warn("[CLIP] embed-await: engine respawned; returning null.");
            LastSearchError = "Search paused while FileID restarts. Try again in a moment.";
            return null;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[CLIP] embed-await threw: {ex.Message}");
            LastSearchError = "Search timed out: " + ex.Message;
            return null;
        }
    }

    private long _engineRankRetryAtTick;

    /// <summary>After a miss the helper is left alone for a few seconds so a busy scan never adds a wait to every keystroke.</summary>
    public async Task<IReadOnlyList<long>?> EngineRankedIdsAsync(CatalogRequest request, CancellationToken ct) =>
        (await EngineRankedAsync(request, ct).ConfigureAwait(false))?.Ids;

    public async Task<(IReadOnlyList<long> Ids, string? Message)?> EngineRankedAsync(CatalogRequest request, CancellationToken ct)
    {
        var engine = EngineClient.Instance;
        if (_disposed || engine.State != EngineClient.LifecycleState.Ready) return null;
        if (Environment.TickCount64 < Interlocked.Read(ref _engineRankRetryAtTick)) return null;
        using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        cts.CancelAfter(EngineSimilarity.EngineWait);
        try
        {
            var response = await engine.CatalogAsync(request, cts.Token).ConfigureAwait(false);
            var ids = EngineSimilarity.RankedIds(response);
            if (ids is null && response.Status != "ok")
            {
                DebugLog.Info($"[CLIP] helper ranking unavailable (pending={EngineSimilarity.IsPending(response)}); ranking in the app.");
                DeferEngineRanking();
            }
            return ids is null ? null : (ids, response.Message);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            DebugLog.Info("[CLIP] helper ranking timed out; ranking in the app.");
            DeferEngineRanking();
            return null;
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[CLIP] helper ranking failed: {ex.GetType().Name}");
            DeferEngineRanking();
            return null;
        }
    }

    private void DeferEngineRanking() =>
        Interlocked.Exchange(ref _engineRankRetryAtTick,
            Environment.TickCount64 + (long)EngineSimilarity.RetryAfterMiss.TotalMilliseconds);

    public async Task<IReadOnlyList<FileRow>> SearchAsync(
        string query, int limit, CancellationToken ct, string? kind = null, Action<string?>? onSummary = null,
        bool showWeaker = false, Action<int>? onHiddenWeaker = null)
    {
        var embed = await EmbedQueryAsync(query, ct).ConfigureAwait(false);
        if (embed != null)
        {
            // It has no kind filter, so a kind-filtered search still ranks here to keep the grid full.
            if (kind is null
                && EngineSimilarity.QueryRequest(query, embed, _store.Scope?.Root.Id, limit) is { } request
                && await EngineRankedAsync(request, ct).ConfigureAwait(false) is { } ranking)
            {
                // No good match: only the name, tag and text matches show, and the helper's weaker look-alikes wait behind a button.
                IReadOnlyList<FileRow>? keyword = null;
                if (!showWeaker && SearchStrengthPolicy.Counts(ranking.Message) is { Good: 0 })
                {
                    keyword = await _store.SearchAsync(query, limit, ct, kind).ConfigureAwait(false);
                    var decision = SearchStrengthPolicy.Decide(query, ranking.Message, keyword.Count, showWeaker);
                    if (decision.KeywordOnly)
                    {
                        onSummary?.Invoke(decision.Label);
                        onHiddenWeaker?.Invoke(decision.HiddenWeaker);
                        return keyword;
                    }
                }
                var engineRows = await _store.RowsByIdsAsync(ranking.Ids, ct).ConfigureAwait(false);
                // The result is not pageable, so "load more" never fetches a copy.
                keyword ??= await _store.SearchAsync(query, limit, ct, kind).ConfigureAwait(false);
                var combined = SearchStrengthPolicy.AppendKeywordRows(engineRows, keyword, row => row.Id);
                if (combined.Count > 0)
                {
                    onSummary?.Invoke(SearchStrengthPolicy.Label(query, ranking.Message));
                    return combined;
                }
            }
            var ranked = await _store.SemanticSearchAsync(embed, limit, ct, kind).ConfigureAwait(false);
            var rows = new List<FileRow>(ranked.Count);
            foreach (var r in ranked)
            {
                rows.Add(r.Row);
            }
            // If the store never opened (DB locked/permission/corrupt — see ReadStore.LastOpenError) the query above returns empty with no signal.
            if (rows.Count == 0 && _store.LastOpenError is { Length: > 0 } storeErr)
            {
                LastSearchError = storeErr;
            }
            return rows;
        }
        var fts = await _store.SearchAsync(query, limit, ct, kind).ConfigureAwait(false);
        if (fts.Count == 0 && _store.LastOpenError is { Length: > 0 } openErr)
        {
            LastSearchError = openErr;
        }
        return fts;
    }
}

/// <summary>Thread-safe bounded LRU mapping a trimmed query string to its CLIP text embedding.</summary>
internal sealed class QueryEmbeddingCache
{
    private readonly int _capacity;
    private readonly object _lock = new();
    private readonly LinkedList<KeyValuePair<string, float[]>> _entries = new();

    public QueryEmbeddingCache(int capacity)
    {
        _capacity = capacity < 1 ? 1 : capacity;
    }

    public int Count
    {
        get { lock (_lock) { return _entries.Count; } }
    }

    public bool TryGet(string key, out float[]? embedding)
    {
        lock (_lock)
        {
            for (var node = _entries.First; node != null; node = node.Next)
            {
                if (node.Value.Key != key) continue;
                embedding = node.Value.Value;
                _entries.Remove(node);
                _entries.AddFirst(node);
                return true;
            }
        }
        embedding = null;
        return false;
    }

    public void Store(string key, float[] embedding)
    {
        lock (_lock)
        {
            for (var node = _entries.First; node != null; node = node.Next)
            {
                if (node.Value.Key == key) { _entries.Remove(node); break; }
            }
            _entries.AddFirst(new KeyValuePair<string, float[]>(key, embedding));
            while (_entries.Count > _capacity)
            {
                _entries.RemoveLast();
            }
        }
    }
}
