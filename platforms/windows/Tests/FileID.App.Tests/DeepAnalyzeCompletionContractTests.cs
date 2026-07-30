using System.Xml.Linq;
using Xunit;

namespace FileID.App.Tests;

public sealed class DeepAnalyzeCompletionContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void SkipCopyAndEngineUseFullCompletionMarker()
    {
        var document = XDocument.Load(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "DeepAnalyze",
            "DeepAnalyzeView.xaml"));
        XNamespace x = "http://schemas.microsoft.com/winfx/2006/xaml";
        var toggle = document
            .Descendants()
            .Single(element => (string?)element.Attribute(x + "Name") == "SkipExistingToggle");
        var help = (string?)toggle.Attribute("AutomationProperties.HelpText");
        Assert.NotNull(help);
        Assert.Contains("completed a full pass", help, StringComparison.Ordinal);
        Assert.Contains("partial passes may run again", help, StringComparison.Ordinal);

        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "engine", "src", "commands",
            "deep_analyze.rs"));
        Assert.Contains("vlm_full_model = ?", commands, StringComparison.Ordinal);

        var pipeline = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "engine", "src", "pipeline",
            "deep_analyze.rs"));
        Assert.Contains("mode.establishes_completion()", pipeline, StringComparison.Ordinal);
        Assert.Contains("vlm_full_model=?1", pipeline, StringComparison.Ordinal);
        Assert.Contains("vlm_full_model=NULL", pipeline, StringComparison.Ordinal);
    }

    private static string PathInRepo(params string[] parts)
        => Path.Combine([RepoRoot, .. parts]);

    private static string FindRepoRoot()
    {
        var current = new DirectoryInfo(AppContext.BaseDirectory);
        while (current is not null)
        {
            if (Directory.Exists(Path.Combine(current.FullName, "platforms"))
                && File.Exists(Path.Combine(current.FullName, "README.md")))
            {
                return current.FullName;
            }
            current = current.Parent;
        }
        throw new DirectoryNotFoundException("FileID repository root not found");
    }
}
