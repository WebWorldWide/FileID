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
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace FileID.Services;

internal sealed class ClipSearchService
{
    private readonly ReadStore _store;

    public ClipSearchService(ReadStore store)
    {
        _store = store;
    }

    /// <summary>
    /// Embeds <paramref name="query"/> via the engine's CLIP text encoder
    /// (Phase 2.6 wires the IPC round-trip). Returns null in Phase 2.4
    /// — the caller falls back to FTS5 search.
    /// </summary>
    public Task<float[]?> EmbedQueryAsync(string query, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(query))
        {
            return Task.FromResult<float[]?>(null);
        }
        // Phase 2.6: send `embedTextQuery` IPC, await engine reply.
        return Task.FromResult<float[]?>(null);
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
