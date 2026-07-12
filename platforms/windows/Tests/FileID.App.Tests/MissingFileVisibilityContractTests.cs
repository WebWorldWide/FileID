using Xunit;

namespace FileID.App.Tests;

public sealed class MissingFileVisibilityContractTests
{
    [Fact]
    public void SoftMissingRowsAreExcludedFromEveryLibrarySummaryQuery()
    {
        var readStore = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Services", "ReadStore.cs"))
            .Replace("\r\n", "\n", StringComparison.Ordinal);
        var restructure = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Restructure", "RestructureView.xaml.cs"))
            .Replace("\r\n", "\n", StringComparison.Ordinal);

        Assert.Contains("WHERE failed = 0\n                  AND vlm_proposed_name", readStore, StringComparison.Ordinal);
        Assert.Contains("SELECT COUNT(*) FROM files WHERE failed = 0 AND vlm_proposed_name", readStore, StringComparison.Ordinal);
        Assert.Contains("SELECT kind, COUNT(*) FROM files WHERE failed = 0 GROUP BY kind", readStore, StringComparison.Ordinal);
        Assert.Contains("FROM files WHERE failed = 0", restructure, StringComparison.Ordinal);
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
