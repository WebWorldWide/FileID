// Shared install-sentinel probe. The engine writes one sentinel per model bundle under `%LOCALAPPDATA%\FileID\Models\.sentinels\`, either flat `{id}.installed` or content-hashed `{id}-{hash}.installed`.
// Every app-side "is it installed?" check must match both forms, or an installed pack re-dispatches its prewarm on every Settings load.

using System;
using System.IO;
using System.Linq;

namespace FileID.Services;

internal static class SentinelProbe
{
    public static bool Installed(string modelId) =>
        InstalledIn(Path.Combine(AppPaths.ModelsDir, ".sentinels"), modelId)
        && RequiredArtifactsPresent(modelId);

    private static bool RequiredArtifactsPresent(string modelId)
        => RequiredArtifactsPresentIn(AppPaths.ModelsDir, modelId);

    internal static bool RequiredArtifactsPresentIn(string root, string modelId)
    {
        bool FilePresent(string relative, long minimumBytes = 1)
        {
            try
            {
                var info = new FileInfo(Path.Combine(root, relative));
                return info.Exists && info.Length >= minimumBytes;
            }
            catch { return false; }
        }
        bool TreeContains(string relative, long minimumBytes, params string[] names)
        {
            try
            {
                var directory = Path.Combine(root, relative);
                if (!Directory.Exists(directory)) return false;
                foreach (var path in Directory.EnumerateFiles(directory, "*", SearchOption.AllDirectories))
                {
                    var name = Path.GetFileName(path);
                    if (names.Contains(name, StringComparer.OrdinalIgnoreCase)
                        && new FileInfo(path).Length >= minimumBytes)
                    {
                        return true;
                    }
                }
                return false;
            }
            catch { return false; }
        }
        bool Vlm(string kind)
        {
            var directory = Path.Combine("vlm", VlmWeightDirs.DirNameFor(kind));
            return FilePresent(Path.Combine(directory, "model.gguf"), 1_048_576)
                && FilePresent(Path.Combine(directory, "mmproj.gguf"), 1_048_576);
        }

        return modelId switch
        {
            "arcface" => FilePresent(Path.Combine("yunet", "face_detection_yunet_2023mar.onnx"), 100_000)
                && FilePresent(Path.Combine("sface", "face_recognition_sface_2021dec.onnx"), 1_000_000),
            "mobileclip_s2" => FilePresent(Path.Combine("mobileclip", "mobileclip_s2_image.onnx"), 1_000_000),
            "clip_text" => FilePresent(Path.Combine("clip_text", "clip_text.onnx"), 1_000_000)
                && FilePresent(Path.Combine("clip_text", "vocab.json"), 1_000)
                && FilePresent(Path.Combine("clip_text", "merges.txt"), 1_000),
            "ram_plus" => FilePresent(Path.Combine("ram_plus", "ram_plus.onnx"), 1_000_000)
                && FilePresent(Path.Combine("ram_plus", "ram_plus_tags.txt"), 1_000)
                && FilePresent(Path.Combine("ram_plus", "ram_plus_thresholds.txt"), 1_000),
            "mistral_small_3_2" or "qwen2_5_vl_7b" or "qwen3_vl_4b"
                or "qwen3_vl_8b" or "gemma_3_4b" => Vlm(modelId),
            "llama_runtime_x64" => TreeContains("llama.cpp", 20_000, "llama-server.exe")
                && TreeContains("llama.cpp", 20_000, "llama-mtmd-cli.exe")
                && TreeContains("llama.cpp", 20_000, "mtmd.dll"),
            // cudnn64_9.dll is a small dispatch shim (~260 KB); a 1 MB floor would report an installed pack as missing.
            "cudnn_runtime_x64" => TreeContains("cudnn", 100_000, "cudnn64_9.dll"),
            // The CUDA pack also ships the runtime the CUDA EP hard-imports (cudart, cublas + cublasLt, cuFFT, NVRTC), matching registry.rs's runtime_required_names. Require all five so a half-extracted pack doesn't read as installed. The 100 KB floor rejects truncated stubs without rejecting cudart64_12.dll (~540 KB).
            "ort_cuda_x64" => TreeContains(Path.Combine("packs", "cuda"), 1_000_000, "onnxruntime.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 1_000_000, "onnxruntime_providers_cuda.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 100_000, "cudart64_12.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 100_000, "cublas64_12.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 100_000, "cublasLt64_12.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 100_000, "cufft64_11.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 100_000, "nvrtc64_120_0.dll"),
            "ort_openvino_x64" => TreeContains(Path.Combine("packs", "openvino"), 1_000_000, "onnxruntime.dll")
                && TreeContains(Path.Combine("packs", "openvino"), 1_000_000, "onnxruntime_providers_openvino.dll"),
            // cudart64_12.dll is a small shim (~540 KB); a 1 MB floor would report an installed pack as missing and re-dispatch its prewarm forever.
            "llama_runtime_cuda_x64" => TreeContains("llama.cpp-cuda", 20_000, "llama-server.exe")
                && TreeContains("llama.cpp-cuda", 20_000, "llama-mtmd-cli.exe")
                && TreeContains("llama.cpp-cuda", 20_000, "mtmd.dll")
                && TreeContains("llama.cpp-cuda", 100_000, "cudart64_12.dll")
                && TreeContains("llama.cpp-cuda", 1_000_000, "cublas64_12.dll"),
            "face_mesh" => FilePresent(Path.Combine("face_mesh", "face_landmarks.onnx"), 1_000_000),
            "bge_text" => FilePresent(Path.Combine("bge_text", "bge_small.onnx"), 1_000_000)
                && FilePresent(Path.Combine("bge_text", "vocab.txt"), 1_000),
            _ => false,
        };
    }

    public static bool InstalledIn(string sentinelsDir, string modelId)
    {
        try
        {
            if (File.Exists(Path.Combine(sentinelsDir, $"{modelId}.installed"))) return true;
            if (!Directory.Exists(sentinelsDir)) return false;
            // `{id}-*` keeps the match exact-id: `arcface` must not match a hypothetical `arcface_xl-{hash}.installed`.
            foreach (var _ in Directory.EnumerateFiles(sentinelsDir, $"{modelId}-*.installed"))
            {
                return true;
            }
            return false;
        }
        catch { return false; }
    }
}
