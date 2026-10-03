using Xunit;

namespace FileID.App.Tests;

public sealed class ReleaseLinkContractTests
{
    [Fact]
    public void PrivacyDocumentIsPackagedWithoutAnExternalFallback()
    {
        var settings = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Settings", "SettingsView.xaml.cs"));
        var publishScript = File.ReadAllText(PathInRepo(
            "platforms", "windows", "build", "publish-store.ps1"));

        Assert.Contains("AppContext.BaseDirectory, \"Docs\", \"PRIVACY.md\"", settings, StringComparison.Ordinal);
        Assert.Contains("shared/docs/PRIVACY.md", publishScript, StringComparison.Ordinal);
        Assert.DoesNotContain("github.com/WebWorldWide/FileID/blob/main/shared/docs/PRIVACY.md", settings, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("developer.nvidia.com/cudnn-downloads", settings, StringComparison.OrdinalIgnoreCase);
    }

    private static string PathInRepo(params string[] parts)
        => Path.Combine([FindRepoRoot(), .. parts]);

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(directory.FullName, "platforms", "windows")))
            {
                return directory.FullName;
            }
        }
        throw new DirectoryNotFoundException("Could not find the FileID repository root from the test output directory.");
    }
}
