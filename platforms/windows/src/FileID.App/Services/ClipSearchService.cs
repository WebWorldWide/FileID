// ClipSearchService — orchestrates CLIP semantic search.
//
// On macOS this lives in `Library/CLIPSearch.swift`. The flow:
//   1. User types a query in the Library search bar (debounced 200 ms).
//   2. App tokenizes + embeds the query via the CLIP text encoder.
//   3. App dot-products against `clip_embeddings.vector` rows in the DB.
//   4. Top-N hits become the Library grid's result set.
//
// Windows mirrors this 1:1 — but the actual text-encoder call lives on
// the engine side (the engine owns the ONNX session that's already loaded
// in VRAM). The app sends a `embedTextQuery` IPC command and the engine
// replies with the L2-normalized 512-d float32 vector. The app then runs
// the dot-product locally via ReadStore.SemanticSearchAsync.
//
// FTS5 fallback returns matching results when the CLIP embedding path
// isn't available; otherwise the engine reply lights up real semantic
// search via the IPC round-trip.

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

    // Bounded LRU of query-string → embedding. The CLIP text encoder is
    // deterministic (same string ⇒ same vector, even across engine respawns),
    // so repeated searches for the same term — kind-filter toggle, clear+retype,
    // nav-back — can skip the IPC round-trip + encode entirely. No
    // generation-based invalidation needed; only a model swap would change the
    // mapping, which is not a supported in-session operation.
    private readonly QueryEmbeddingCache _queryCache = new(capacity: 24);

    // generation counter, bumped each time the engine's lifecycle
    // transitions away from Ready (respawn or crash). Each in-flight TCS
    // captures the generation it was created in; an embedding that arrives
    // from a stale generation completes the wrong caller's TCS, which we
    // detect via the prefix and discard.
    private int _generation;

    /// <summary>Last non-cancellation error from a search round-trip. Null
    /// when the most recent search succeeded. Bind in the search box UI to
    /// surface "Search unavailable" instead of silently empty results.</summary>
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
        // Drain any in-flight requests so callers don't hang.
        foreach (var kv in _inflight)
        {
            kv.Value.TrySetResult(null);
        }
        _inflight.Clear();
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ClipSearchService.OnEngineClientChanged", () =>
        {
            // This handler can fire after Dispose() — Dispose() detaches the
            // subscription, but a notification already in flight on another
            // thread (tab-nav disposal racing an engine state change) can land
            // here with _disposed already true. Bail before touching _inflight
            // (drained + cleared in Dispose) or the now-stale generation so we
            // don't complete a disposed caller's TCS with stale/null results.
            if (_disposed) return;
            if (e.PropertyName == nameof(EngineClient.State))
            {
                DebugLog.Debug($"[ENGINE-SUB:ClipSearchService] {e.PropertyName}");
                // any State transition (respawn / crash / shutdown)
                // invalidates pending embeddings — the engine that received
                // the embedTextQuery is now gone. Bump the generation +
                // fault all in-flight TCSes so callers don't hang.
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
            DebugLog.Debug($"[ENGINE-SUB:ClipSearchService] {e.PropertyName}");
            var emb = EngineClient.Instance.LastClipTextEmbedding;
            if (emb is null) return;
            if (_inflight.TryRemove(emb.QueryId, out var tcs))
            {
                tcs.TrySetResult(emb.Embedding?.ToArray());
            }
        });

    /// <summary>
    /// Sends an `embedTextQuery` IPC command and awaits the engine's
    /// `clipTextEmbedding` reply (correlated by query_id). Returns null
    /// if the engine times out or CLIP isn't installed (the caller falls
    /// back to FTS5 search transparently).
    /// </summary>
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
            // Engine I/O / serialization failure. Surface to UI via
            // LastSearchError instead of silently returning empty results.
            _inflight.TryRemove(queryId, out _);
            DebugLog.Warn($"[CLIP] EmbedTextQueryAsync threw: {ex.Message}");
            LastSearchError = "Search unavailable: " + ex.Message;
            return null;
        }

        // 5-second timeout: a healthy CLIP encode is sub-100ms, so 5s is
        // generous slack for cold-start ORT session create.
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
            // Empty embedding = CLIP disabled engine-side —
            // treat as null so SearchAsync falls back to FTS5, no error banner.
            if (result is { Length: 0 }) { LastSearchError = null; return null; }
            // Successful round-trip — clear any stale error banner + cache the
            // deterministic embedding so an identical query skips the next encode.
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
            // engine died mid-query. Surface a clear banner; the
            // caller (LibraryView search) treats null as "fall through to
            // FTS5" so the user still gets results.
            DebugLog.Warn("[CLIP] embed-await: engine respawned; returning null.");
            LastSearchError = "Search paused — engine restarting.";
            return null;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[CLIP] embed-await threw: {ex.Message}");
            LastSearchError = "Search timed out: " + ex.Message;
            return null;
        }
    }

    public async Task<IReadOnlyList<FileRow>> SearchAsync(
        string query, int limit, CancellationToken ct, string? kind = null)
    {
        var embed = await EmbedQueryAsync(query, ct).ConfigureAwait(false);
        if (embed != null)
        {
            var ranked = await _store.SemanticSearchAsync(embed, limit, ct, kind).ConfigureAwait(false);
            var rows = new List<FileRow>(ranked.Count);
            foreach (var r in ranked)
            {
                rows.Add(r.Row);
            }
            // If the store never opened (DB locked/permission/corrupt — see
            // ReadStore.LastOpenError) the query above returns empty with no
            // signal. Surface the store's open error so search shows a clear,
            // dismissible message instead of an indistinguishable empty grid.
            if (rows.Count == 0 && _store.LastOpenError is { Length: > 0 } storeErr)
            {
                LastSearchError = storeErr;
            }
            return rows;
        }
        // FTS5 fallback (filename + OCR) — covers the case where CLIP
        // models aren't installed yet OR the query embedded to all-zeros.
        var fts = await _store.SearchAsync(query, limit, ct, kind).ConfigureAwait(false);
        if (fts.Count == 0 && _store.LastOpenError is { Length: > 0 } openErr)
        {
            LastSearchError = openErr;
        }
        return fts;
    }
}

/// <summary>Thread-safe bounded LRU mapping a trimmed query string to its CLIP
/// text embedding. A hit moves the entry to the front (most-recently-used); a
/// store dedupes the key then evicts the oldest beyond <c>capacity</c>. Extracted
/// from <see cref="ClipSearchService"/> so the hit/evict policy is unit-testable
/// without the engine singleton.</summary>
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
