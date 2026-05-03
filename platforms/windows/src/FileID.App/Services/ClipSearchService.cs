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
// Phase 2.4 cut: the wiring + a stubbed embedding path that returns the
// FTS5 fallback set so the UI is exercisable end-to-end. Phase 2.6
// (engine ML wiring complete) lights up the real CLIP IPC round-trip.

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

internal sealed class ClipSearchService : IDisposable
{
    private readonly ReadStore _store;
    private readonly ConcurrentDictionary<string, TaskCompletionSource<float[]?>> _inflight = new();
    private bool _disposed;

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
    {
        if (e.PropertyName != nameof(EngineClient.LastClipTextEmbedding)) return;
        var emb = EngineClient.Instance.LastClipTextEmbedding;
        if (emb is null) return;
        if (_inflight.TryRemove(emb.QueryId, out var tcs))
        {
            tcs.TrySetResult(emb.Embedding?.ToArray());
        }
    }

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
        var queryId = Guid.NewGuid().ToString("N");
        var tcs = new TaskCompletionSource<float[]?>(TaskCreationOptions.RunContinuationsAsynchronously);
        _inflight[queryId] = tcs;
        try
        {
            await EngineClient.Instance.EmbedTextQueryAsync(query, queryId).ConfigureAwait(false);
        }
        catch (Exception)
        {
            _inflight.TryRemove(queryId, out _);
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
            return await tcs.Task.ConfigureAwait(false);
        }
        catch
        {
            return null;
        }
    }

    public async Task<IReadOnlyList<FileRow>> SearchAsync(
        string query, int limit, CancellationToken ct)
    {
        var embed = await EmbedQueryAsync(query, ct).ConfigureAwait(false);
        if (embed != null)
        {
            var ranked = await _store.SemanticSearchAsync(embed, limit, ct).ConfigureAwait(false);
            var rows = new List<FileRow>(ranked.Count);
            foreach (var r in ranked)
            {
                rows.Add(r.Row);
            }
            return rows;
        }
        // FTS5 fallback (filename + OCR) — covers the case where CLIP
        // models aren't installed yet OR the query embedded to all-zeros.
        return await _store.SearchAsync(query, limit, ct).ConfigureAwait(false);
    }
}
