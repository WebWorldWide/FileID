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
        "gemma_3_4b" => "gemma-3-4b",
        _ => kind,
    };
}
