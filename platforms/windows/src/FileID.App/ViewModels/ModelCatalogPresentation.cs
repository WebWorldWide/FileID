using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Filtering, ordering and wording for the model catalog, shared by the
/// Models workbench and its tests. Mirrors the GTK catalog section.</summary>
internal static class ModelCatalogPresentation
{
    public static readonly IReadOnlyList<(string Label, string Role)> Roles =
        new[] { ("Vision", "vision"), ("Text", "text"), ("Embedding", "embedding"), ("Speech", "speech") };

    private const int MaxRows = 80;

    /// <summary>Measured models first (fastest decode first), then curated, then smallest resident estimate.</summary>
    public static IReadOnlyList<ModelCatalogItem> Visible(IEnumerable<ModelCatalogItem> models, bool onlyFit) =>
        models.Where(m => !onlyFit || m.Fits)
            .OrderByDescending(m => m.Benchmark?.GeneratedTokensPerSecond ?? -1)
            .ThenByDescending(m => m.Status == "curated")
            .ThenBy(m => m.EstimatedResidentGB)
            .ThenBy(m => m.Id, StringComparer.Ordinal)
            .Take(MaxRows)
            .ToArray();

    public static string Summary(ModelCatalogItem model)
    {
        var terms = model.TermsRequired ? " · terms shown before install" : "";
        var state = model.Installed ? "installed" : model.Runnable ? "available" : "not yet runnable here";
        return FormattableString.Invariant(
            $"{model.Name} · {model.Tier} · {model.EstimatedResidentGB:0.0} GB resident · {model.DownloadBytes / 1e9:0.0} GB download · {model.License} · {model.Status} · {state}{terms}");
    }

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
    public static bool CanInstall(ModelCatalogItem model) => model.Runnable && !model.Installed;
}
