// Shared install-sentinel probe. The engine writes one sentinel per
// installed model bundle under `%LOCALAPPDATA%\FileID\Models\.sentinels\`
// as either flat `{id}.installed` or content-hashed `{id}-{hash}.installed`
// (atomic temp+rename; see engine main.rs handle_prewarm_model). Every
// app-side "is it installed?" check must match BOTH forms — flat-only
// probes in SettingsView and the auto-installers never saw the hashed
// sentinels, so an already-installed CUDA runtime pack re-dispatched its
// prewarm on every Settings load / engine Ready (repeated 0%→100% progress
// spam with outcome=already_installed).

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
            "mistral_small_3_2" or "qwen2_5_vl_7b" or "gemma_3_4b" => Vlm(modelId),
            "llama_runtime_x64" => TreeContains("llama.cpp", 20_000, "llama-server.exe")
                && TreeContains("llama.cpp", 20_000, "llama-mtmd-cli.exe")
                && TreeContains("llama.cpp", 20_000, "mtmd.dll"),
            "whisper" => TreeContains("whisper.cpp", 20_000, "main.exe", "whisper-cli.exe")
                && FilePresent(Path.Combine("whisper", "ggml-base.bin"), 1_000_000),
            // cudnn64_9.dll is the small cuDNN 9 dispatch shim (~260 KB); the real
            // ops live in cudnn_ops64_9.dll / cudnn_cnn64_9.dll. A 1 MB floor made
            // this probe report an installed cuDNN pack as missing (same bug shape
            // as cudart64_12.dll above).
            "cudnn_runtime_x64" => TreeContains("cudnn", 100_000, "cudnn64_9.dll"),
            "ort_cuda_x64" => TreeContains(Path.Combine("packs", "cuda"), 1_000_000, "onnxruntime.dll")
                && TreeContains(Path.Combine("packs", "cuda"), 1_000_000, "onnxruntime_providers_cuda.dll"),
            "ort_openvino_x64" => TreeContains(Path.Combine("packs", "openvino"), 1_000_000, "onnxruntime.dll")
                && TreeContains(Path.Combine("packs", "openvino"), 1_000_000, "onnxruntime_providers_openvino.dll"),
            "llama_runtime_cuda_x64" => TreeContains("llama.cpp-cuda", 20_000, "llama-server.exe")
                && TreeContains("llama.cpp-cuda", 20_000, "llama-mtmd-cli.exe")
                && TreeContains("llama.cpp-cuda", 20_000, "mtmd.dll")
                // cudart64_12.dll is CUDA's small runtime shim (~540 KB), NOT a
                // multi-MB math lib — a 1 MB floor here made SentinelProbe report
                // the fully-installed CUDA pack as missing, so the app re-dispatched
                // its prewarm forever (0%→100% "already_installed" spam → the
                // "installer stopped reporting progress" timeout). Guard against a
                // truncated/stub file without rejecting the real one.
                && TreeContains("llama.cpp-cuda", 100_000, "cudart64_12.dll")
                && TreeContains("llama.cpp-cuda", 1_000_000, "cublas64_12.dll"),
            "bge_text" => FilePresent(Path.Combine("bge_text", "bge_small.onnx"), 1_000_000)
                && FilePresent(Path.Combine("bge_text", "vocab.txt"), 1_000),
            "florence2_base" => FilePresent(Path.Combine("florence2", "vision_encoder.onnx"), 1_000_000)
                && FilePresent(Path.Combine("florence2", "embed_tokens.onnx"), 1_000_000)
                && FilePresent(Path.Combine("florence2", "encoder_model.onnx"), 1_000_000)
                && FilePresent(Path.Combine("florence2", "decoder_model_merged.onnx"), 1_000_000)
                && FilePresent(Path.Combine("florence2", "tokenizer.json"), 1_000)
                && FilePresent(Path.Combine("florence2", "config.json"), 1_000),
            _ => false,
        };
    }

    public static bool InstalledIn(string sentinelsDir, string modelId)
    {
        try
        {
            if (File.Exists(Path.Combine(sentinelsDir, $"{modelId}.installed"))) return true;
            if (!Directory.Exists(sentinelsDir)) return false;
            // `{id}-*` keeps the match exact-id: `arcface` must not match a
            // hypothetical `arcface_xl-{hash}.installed`.
            foreach (var _ in Directory.EnumerateFiles(sentinelsDir, $"{modelId}-*.installed"))
            {
                return true;
            }
            return false;
        }
        catch { return false; }
    }
}
