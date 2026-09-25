// VlmWeightDirs — wire model_kind → on-disk weights dir under Models\vlm\.
//
// The engine's registry (engine/src/models/registry.rs) installs each VLM
// into a dotted/kebab dir ("vlm/mistral-small-3.2") while the wire kind the
// app passes around is snake_case ("mistral_small_3_2"). Probing
// vlm\<kind> directly reported every installed VLM as missing (the same
// defect class as the engine's vlm::find_weights bug).

namespace FileID.Services;

internal static class VlmWeightDirs
{
    /// <summary>Registry dir name for a Deep Analyze model_kind. Unknown kinds
    /// pass through unchanged so a caller already holding the dir spelling
    /// still resolves.</summary>
    internal static string DirNameFor(string kind) => kind switch
    {
        "mistral_small_3_2" => "mistral-small-3.2",
        "qwen2_5_vl_7b" => "qwen2.5-vl-7b",
        "qwen3_vl_4b" => "qwen3-vl-4b",
        "qwen3_vl_8b" => "qwen3-vl-8b",
        "gemma_3_4b" => "gemma-3-4b",
        _ => kind,
    };

    internal static bool WeightsPresent(string modelsDir, string kind)
    {
        var dir = System.IO.Path.Combine(modelsDir, "vlm", DirNameFor(kind));
        return Complete(System.IO.Path.Combine(dir, "model.gguf"))
            && Complete(System.IO.Path.Combine(dir, "mmproj.gguf"));

        static bool Complete(string path)
        {
            var file = new System.IO.FileInfo(path);
            return file.Exists && file.Length >= 1_048_576;
        }
    }
}
