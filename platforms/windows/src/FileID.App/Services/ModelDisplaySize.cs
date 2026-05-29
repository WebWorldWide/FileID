// ModelDisplaySize — single source of truth for the approximate on-disk
// size of each model_kind. Mirrors engine/src/models/registry.rs's
// `approx_bytes` totals (summed across all FileEntry rows for that kind).
// Welcome sheet + Settings model cards + any size-related test reads here.

using System.Collections.Generic;

namespace FileID.Services;

internal static class ModelDisplaySize
{
    /// <summary>Sum of `approx_bytes` (in megabytes) for every file the
    /// engine downloads for the given model_kind. Used to render
    /// "MobileCLIP-S2 — 143 MB" style labels in the Welcome sheet.
    /// Returns 0 for unknown model_kinds — callers can use the absence
    /// as a "don't show a size badge" signal.</summary>
    public static int GetDisplaySizeMB(string modelKind) =>
        TotalsByKind.TryGetValue(modelKind, out var mb) ? mb : 0;

    /// <summary>Total bytes from engine registry, expressed as MB so the
    /// downstream label code can format with thousands separators without
    /// dealing with ulong arithmetic.</summary>
    private static readonly IReadOnlyDictionary<string, int> TotalsByKind =
        new Dictionary<string, int>
        {
            // (2_300_000_000 + 870_000_000) / 1_048_576 ≈ 3023 MB
            // The spec uses decimal MB (1e6); registry numbers are decimal too,
            // so divide by 1_000_000 for the displayed value.
            ["qwen2_5_vl_7b"] = (4_700 + 1_400),       // = 6100
            ["gemma_3_4b"] = (2_500 + 851),          // = 3351
            // Mistral-Small-3.2-24B Q4_K_M (~14.3 GB) + mmproj (~878 MB).
            ["mistral_small_3_2"] = (14_300 + 878),    // = 15178
            // RAM++ ONNX (Swin-L @384, fp16) ~882 MB + tiny tag/threshold sidecars.
            ["ram_plus"] = 926,
            ["arcface"] = (174 + 17),             // ≈ 191 (matches registry sums)
            ["mobileclip_s2"] = 143,
            ["clip_text"] = (254 + 1 + 1),          // ≈ 256 with vocab + merges
            ["cudnn_runtime_x64"] = 430,
        };
}
