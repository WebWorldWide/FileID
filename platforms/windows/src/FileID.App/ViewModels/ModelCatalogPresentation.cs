using System.Text.RegularExpressions;
using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>What the model browser shows: search text, the hardware-fit filter, downloaded
/// only, and whether only models Describe Files can run are listed.</summary>
internal sealed record ModelCatalogFilter(string? Query = null, bool OnlyFit = true, bool InstalledOnly = false, bool DescribeFilesOnly = false);

/// <summary>Filtering, ordering and wording for the model catalog browser (Describe Files and
/// Settings) and its tests.</summary>
internal static partial class ModelCatalogPresentation
{
    public static readonly IReadOnlyList<(string Label, string? Role)> Roles =
        new (string, string?)[] { ("All", null), ("Vision", "vision"), ("Text", "text"), ("Embedding", "embedding"), ("Speech", "speech") };

    public const int MaxRows = 80;

    public const string GemmaTermsNote = "Google Gemma Terms of Use apply. You review and accept them before the download starts.";

    /// <summary>Permissive weights only; Gemma is offered solely as an opt-in with its terms shown.</summary>
    public static bool IsOfferable(ModelCatalogItem model) =>
        model.License is "Apache-2.0" or "MIT" || (model.License == "Gemma" && model.TermsRequired);

    /// <summary>The license policy a download must accept first, or null for permissive weights.</summary>
    public static string? TermsPolicyKey(ModelCatalogItem model) => model.License == "Gemma" ? "Gemma" : null;

    public static bool IsDescribeFilesModel(ModelCatalogItem model) =>
        model.Role == "vision" && model.Format == "gguf" && model.Runnable;

    /// <summary>A catalog GGUF id (as the engine validates them); the engine still rejects unknown ids.</summary>
    public static bool IsCatalogModelKind(string? id) =>
        id is { Length: > 0 and <= 120 } && CatalogIdPattern().IsMatch(id) && id.Contains("_gguf", StringComparison.Ordinal);

    /// <summary>Every offerable model matching the filter, curated defaults first, then measured
    /// models (fastest decode first), then smallest resident estimate.</summary>
    public static IReadOnlyList<ModelCatalogItem> Filter(IEnumerable<ModelCatalogItem> models, ModelCatalogFilter filter)
    {
        var query = filter.Query?.Trim() ?? "";
        return models
            .Where(IsOfferable)
            .Where(m => !filter.OnlyFit || m.Fits)
            .Where(m => !filter.InstalledOnly || m.Installed)
            .Where(m => !filter.DescribeFilesOnly || IsDescribeFilesModel(m))
            .Where(m => query.Length == 0 || Matches(m, query))
            .OrderByDescending(m => m.Status == "curated")
            .ThenByDescending(m => m.Benchmark?.GeneratedTokensPerSecond ?? -1)
            .ThenBy(m => m.EstimatedResidentGB)
            .ThenBy(m => m.Id, StringComparer.Ordinal)
            .ToArray();
    }

    public static IReadOnlyList<ModelCatalogItem> Visible(IEnumerable<ModelCatalogItem> models, ModelCatalogFilter filter) =>
        Filter(models, filter).Take(MaxRows).ToArray();

    public static IReadOnlyList<ModelCatalogItem> Visible(IEnumerable<ModelCatalogItem> models, bool onlyFit) =>
        Visible(models, new ModelCatalogFilter(OnlyFit: onlyFit));

    private static bool Matches(ModelCatalogItem model, string query) =>
        model.Name.Contains(query, StringComparison.OrdinalIgnoreCase)
        || model.Upstream.Contains(query, StringComparison.OrdinalIgnoreCase)
        || model.Family.Contains(query, StringComparison.OrdinalIgnoreCase)
        || model.License.Contains(query, StringComparison.OrdinalIgnoreCase)
        || model.Id.Contains(query, StringComparison.OrdinalIgnoreCase);

    public static string HardwareSummary(ModelCatalog catalog)
    {
        var vram = catalog.VramMB > 0
            ? FormattableString.Invariant($"{catalog.VramMB / 1024.0:0.#} GB dedicated graphics memory")
            : "no dedicated graphics memory reported";
        return FormattableString.Invariant($"This PC: {catalog.PhysicalMemoryGB:0} GB memory · {vram}");
    }

    public static string CountSummary(int shown, int matching) =>
        matching > shown
            ? FormattableString.Invariant($"Showing {shown} of {matching} models. Search to narrow the list.")
            : matching == 1 ? "1 model" : FormattableString.Invariant($"{matching} models");

    public static string Summary(ModelCatalogItem model)
    {
        var terms = model.TermsRequired ? " · terms shown before install" : "";
        var state = model.Installed ? "installed" : model.Runnable ? "available" : "not yet runnable here";
        return FormattableString.Invariant(
            $"{model.Name} · {model.Tier} · {model.EstimatedResidentGB:0.0} GB resident · {model.DownloadBytes / 1e9:0.0} GB download · {model.License} · {model.Status} · {state}{terms}");
    }

    public static string? FitNote(ModelCatalogItem model) => model.Fits ? null : "Needs more memory than this PC has";

    public static string BenchmarkSummary(ModelBenchmark benchmark)
    {
        var parts = new List<string> { FormattableString.Invariant($"loads in {benchmark.LoadSeconds:0.0} s") };
        if (benchmark.GeneratedTokensPerSecond is { } rate) parts.Add(FormattableString.Invariant($"{rate:0} tok/s"));
        if (benchmark.SecondsPerImage is { } seconds) parts.Add(FormattableString.Invariant($"{seconds:0.0} s/image"));
        if (benchmark.ProbeAccuracy is { } accuracy) parts.Add(FormattableString.Invariant($"{accuracy * 100:0}% probes"));
        if (benchmark.JsonValidRate is { } json) parts.Add(FormattableString.Invariant($"{json * 100:0}% valid JSON"));
        if (benchmark.PeakResidentGB is { } peak) parts.Add(FormattableString.Invariant($"{peak:0.0} GB peak"));
        if (benchmark.TunedArguments is { Count: > 0 }) parts.Add("tuned");
        return "Measured here: " + string.Join(" · ", parts);
    }

    public static bool CanBenchmark(ModelCatalogItem model) => model.Installed && model.Format == "gguf";
    public static bool CanInstall(ModelCatalogItem model) => model.Runnable && !model.Installed && IsOfferable(model);
    public static bool CanUseForDescribeFiles(ModelCatalogItem model) => IsDescribeFilesModel(model) && model.Fits && model.Installed;

    [GeneratedRegex("^[a-z0-9_]+$", RegexOptions.CultureInvariant)]
    private static partial Regex CatalogIdPattern();
}
