// Approximate on-disk size per model_kind, matching the engine registry's `approx_bytes` totals.

using System.Collections.Generic;

namespace FileID.Services;

internal static class ModelDisplaySize
{
    public static int GetDisplaySizeMB(string modelKind) =>
        TotalsByKind.TryGetValue(modelKind, out var mb) ? mb : 0;

    /// <summary>Total size in MB, summed across the kind's files in the engine registry.</summary>
    private static readonly IReadOnlyDictionary<string, int> TotalsByKind =
        new Dictionary<string, int>
        {
            ["qwen2_5_vl_7b"] = (4_700 + 1_400),
            ["qwen3_vl_4b"] = 3_334,
            ["qwen3_vl_8b"] = 6_187,
            ["gemma_3_4b"] = (2_500 + 851),
            ["mistral_small_3_2"] = (14_300 + 878),
            ["ram_plus"] = 926,
            ["arcface"] = 37,                     // YuNet (~0.2 MB) + SFace (~37 MB)
            ["face_mesh"] = 5,                    // Face Mesh landmarks ONNX (~4.9 MB, Apache-2.0)
            ["mobileclip_s2"] = 352,        // CLIP ViT-B/32 vision ONNX (~335 MB, MIT)
            ["clip_text"] = (254 + 1 + 1),          // ≈ 256 with vocab + merges
            ["cudnn_runtime_x64"] = 430,
            ["ort_cuda_x64"] = 1_164,
            ["llama_runtime_cuda_x64"] = 651,
            ["ort_openvino_x64"] = 40,
        };
}
