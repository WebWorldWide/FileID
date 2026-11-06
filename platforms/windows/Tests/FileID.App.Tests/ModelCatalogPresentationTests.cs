using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ModelCatalogPresentationTests
{
    private static readonly string[] TunedArguments = { "-t", "4" };
    private static readonly string[] ExpectedOrder = { "curated", "fast", "slow", "tiny", "big" };
    private static readonly string[] OfferedOrder = { "mit", "gemma_3_4b_it_gguf_q4_k_m" };

    private static ModelCatalogItem Model(string id, bool fits, string status, double resident, double? rate, bool installed = false) =>
        new(id, id, "family", $"org/{id}", "vision", "gguf", "Q4_K_M", "Apache-2.0", false, null, 2_000_000_000, resident,
            "small", status, true, installed || rate is not null, fits,
            rate is { } r ? new ModelBenchmark("t", "h", 2.0, null, r, 1.5, 2.4, 0.5, 0.8, TunedArguments) : null);

    [Fact]
    public void CuratedDefaultsLeadThenMeasuredThenSmallest()
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
        Assert.Equal(ExpectedOrder, ModelCatalogPresentation.Visible(models, onlyFit: true).Select(m => m.Id));
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
    public void NonCommercialWeightsAreNeverOfferedAndGemmaNeedsItsTerms()
    {
        var gemma = Model("gemma_3_4b_it_gguf_q4_k_m", true, "candidate", 4, null) with { License = "Gemma", TermsRequired = true };
        var unaccepted = gemma with { TermsRequired = false };
        var research = Model("research", true, "candidate", 2, null) with { License = "Qwen-Research" };
        var mit = Model("mit", true, "candidate", 2, null) with { License = "MIT" };
        var shown = ModelCatalogPresentation.Visible(new[] { gemma, unaccepted, research, mit }, onlyFit: false).Select(m => m.Id).ToArray();
        Assert.Equal(OfferedOrder, shown);
        Assert.Equal("Gemma", ModelCatalogPresentation.TermsPolicyKey(gemma));
        Assert.Null(ModelCatalogPresentation.TermsPolicyKey(mit));
        Assert.False(ModelCatalogPresentation.CanInstall(research));
        Assert.Contains("Gemma Terms of Use", ModelCatalogPresentation.GemmaTermsNote, StringComparison.Ordinal);
    }

    [Fact]
    public void DescribeFilesBrowserListsOnlyRunnableVisionThatFitsBySearch()
    {
        var vision = Model("qwen3_vl_2b_instruct_gguf_q4_k_m", true, "candidate", 2, null) with { Name = "Qwen3-VL 2B" };
        var text = vision with { Id = "qwen3_4b_gguf_q4_k_m", Name = "Qwen3 4B", Role = "text", Runnable = false };
        var tooBig = vision with { Id = "big_vl_gguf_q4_k_m", Name = "Big VL", Fits = false };
        var filter = new ModelCatalogFilter(Query: "qwen", OnlyFit: true, DescribeFilesOnly: true);
        Assert.Equal(new[] { vision.Id }, ModelCatalogPresentation.Filter(new[] { vision, text, tooBig }, filter).Select(m => m.Id));
        Assert.Empty(ModelCatalogPresentation.Filter(new[] { vision }, filter with { Query = "mistral" }));
        Assert.True(ModelCatalogPresentation.CanUseForDescribeFiles(vision with { Installed = true }));
        Assert.False(ModelCatalogPresentation.CanUseForDescribeFiles(vision));
        Assert.False(ModelCatalogPresentation.CanUseForDescribeFiles(tooBig with { Installed = true }));
    }

    [Fact]
    public void ListIsCappedWithACountAndHardwareReadsInGigabytes()
    {
        var many = Enumerable.Range(0, 120).Select(i => Model($"m{i:000}", true, "candidate", i, null)).ToArray();
        Assert.Equal(ModelCatalogPresentation.MaxRows, ModelCatalogPresentation.Visible(many, onlyFit: true).Count);
        Assert.Equal("Showing 80 of 120 models. Search to narrow the list.", ModelCatalogPresentation.CountSummary(80, 120));
        Assert.Equal("1 model", ModelCatalogPresentation.CountSummary(1, 1));
        Assert.Equal("This PC: 32 GB memory · 8 GB dedicated graphics memory",
            ModelCatalogPresentation.HardwareSummary(new ModelCatalog("r", 32, 8192, Array.Empty<ModelCatalogItem>())));
        Assert.Equal("This PC: 16 GB memory · no dedicated graphics memory reported",
            ModelCatalogPresentation.HardwareSummary(new ModelCatalog("r", 16, 0, Array.Empty<ModelCatalogItem>())));
    }

    [Theory]
    [InlineData("gemma_3_4b_it_gguf_q4_k_m", true)]
    [InlineData("smolvlm_256m_instruct_gguf_q8_0", true)]
    [InlineData("qwen2_5_vl_3b", false)]
    [InlineData("qwen2_5_vl_7b", false)]
    [InlineData("../evil_gguf", false)]
    [InlineData("Upper_gguf_q4", false)]
    [InlineData("", false)]
    public void CatalogIdsFollowTheEngineGrammar(string id, bool expected)
    {
        Assert.Equal(expected, ModelCatalogPresentation.IsCatalogModelKind(id));
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
