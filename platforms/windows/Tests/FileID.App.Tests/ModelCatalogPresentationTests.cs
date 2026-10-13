using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ModelCatalogPresentationTests
{
    private static ModelCatalogItem Model(string id, bool fits, string status, double resident, double? rate, bool installed = false) =>
        new(id, id, "family", $"org/{id}", "vision", "gguf", "Q4_K_M", "Apache-2.0", false, null, 2_000_000_000, resident,
            "small", status, true, installed || rate is not null, fits,
            rate is { } r ? new ModelBenchmark("t", "h", 2.0, null, r, 1.5, 2.4, 0.5, 0.8, new[] { "-t", "4" }) : null);

    [Fact]
    public void MeasuredModelsLeadThenCuratedThenSmallest()
    {
        var models = new[]
        {
            Model("big", true, "candidate", 9, null),
            Model("curated", true, "curated", 7, null),
            Model("slow", true, "verified", 3, 10),
            Model("fast", true, "verified", 5, 40),
            Model("tiny", true, "candidate", 1, null),
            Model("huge", false, "candidate", 60, null),
        };
        Assert.Equal(new[] { "fast", "slow", "curated", "tiny", "big" }, ModelCatalogPresentation.Visible(models, onlyFit: true).Select(m => m.Id));
        Assert.Equal(6, ModelCatalogPresentation.Visible(models, onlyFit: false).Count);
    }

    [Fact]
    public void SummariesStateLicenseStatusAndMeasurements()
    {
        var measured = Model("fast", true, "verified", 5, 40);
        Assert.Contains("Apache-2.0 · verified · installed", ModelCatalogPresentation.Summary(measured));
        Assert.Equal("Measured here: loads in 2.0 s · 40 tok/s · 1.5 s/image · 80% probes · 50% valid JSON · 2.4 GB peak · tuned",
            ModelCatalogPresentation.BenchmarkSummary(measured.Benchmark!));
        var gemma = Model("g", true, "candidate", 4, null) with { License = "Gemma", TermsRequired = true };
        Assert.EndsWith("terms shown before install", ModelCatalogPresentation.Summary(gemma));
    }

    [Fact]
    public void ActionsFollowInstallStateAndFormat()
    {
        var available = Model("a", true, "candidate", 2, null);
        Assert.True(ModelCatalogPresentation.CanInstall(available));
        Assert.False(ModelCatalogPresentation.CanBenchmark(available));
        var installed = available with { Installed = true };
        Assert.False(ModelCatalogPresentation.CanInstall(installed));
        Assert.True(ModelCatalogPresentation.CanBenchmark(installed));
        Assert.False(ModelCatalogPresentation.CanBenchmark(installed with { Format = "mlx" }));
        Assert.False(ModelCatalogPresentation.CanInstall(available with { Runnable = false }));
    }

    [Fact]
    public void BenchmarkCommandsRoundTripWithTheirCorrelationID()
    {
        var json = IpcCoder.Encode(new IpcCommand("envelope", new BenchmarkModelCommand("bench-1", "smolvlm_256m_instruct_gguf_q8_0", true)));
        Assert.Contains("\"benchmarkModel\":{", json);
        var decoded = Assert.IsType<BenchmarkModelCommand>(IpcCoder.Decode<IpcCommand>(json).Payload);
        Assert.Equal(("bench-1", "smolvlm_256m_instruct_gguf_q8_0", true), (decoded.RequestId, decoded.ModelKind, decoded.Tune));
        Assert.Contains("\"cancelBenchmark\":{}", IpcCoder.Encode(new IpcCommand("envelope", new CancelBenchmarkCommand())));
    }
}
