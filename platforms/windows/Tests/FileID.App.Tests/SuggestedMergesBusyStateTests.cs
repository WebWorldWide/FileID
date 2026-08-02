using System.Xml.Linq;
using Xunit;

namespace FileID.App.Tests;

/// The Suggested-merges sheet fetches over every person cluster (3,014 on a real
/// library) and the engine emits only a TERMINAL mergeSuggestions event — no
/// per-step progress. Without a visible busy state the sheet showed one static
/// line of text for several seconds and read as frozen/broken.
///
/// Source-derived so it fails if the indicator is removed or stops being cleared
/// on an exit path, which is how this reverts to "looks hung" without anyone
/// noticing.
public sealed class SuggestedMergesBusyStateTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    private static string SheetXaml() => Path.Combine(
        RepoRoot, "platforms", "windows", "src", "FileID.App",
        "Views", "People", "SuggestedMergesSheet.xaml");

    private static string SheetCode() => SheetXaml() + ".cs";

    [Fact]
    public void SheetDeclaresAnIndeterminateBusyIndicator()
    {
        var root = Assert.IsType<XElement>(XDocument.Load(SheetXaml()).Root);
        var xaml = root.GetDefaultNamespace();
        var name = XName.Get("Name", "http://schemas.microsoft.com/winfx/2006/xaml");

        var ring = Assert.Single(
            root.Descendants(xaml + "ProgressRing"),
            e => e.Attribute(name)?.Value == "BusyRing");

        // A ProgressRing is indeterminate by nature. Assert no one has bolted a
        // fabricated percentage onto it — the engine reports no progress, so any
        // determinate value would be invented.
        Assert.Null(ring.Attribute("Value"));
        Assert.Null(ring.Attribute("IsIndeterminate"));

        Assert.Contains(root.Descendants(),
            e => e.Attribute(name)?.Value == "BusyPanel");
    }

    [Fact]
    public void EveryExitPathClearsTheBusyState()
    {
        var code = File.ReadAllText(SheetCode());

        // Entered exactly once, when the fetch starts.
        Assert.Equal(1, CountOf(code, "SetBusy(true"));

        // Cleared on success, timeout, and error — three awaited exit paths in the
        // Loaded handler — plus once in Render() for the reply-driven path. A
        // missing clear leaves the ring spinning forever over stale content.
        Assert.True(CountOf(code, "SetBusy(false") >= 4,
            "SetBusy(false) must run on the success, timeout, and error paths and in Render()");

        // Render() must not clear the busy state on a null (not-yet-arrived)
        // result, or the ring vanishes and "No merge-review candidates found." flashes
        // before the real reply lands.
        var render = code[code.IndexOf("private void Render()", StringComparison.Ordinal)..];
        var nullGuard = render.IndexOf("if (sug is null) return;", StringComparison.Ordinal);
        var clear = render.IndexOf("SetBusy(false)", StringComparison.Ordinal);
        Assert.True(nullGuard >= 0, "Render must early-return on a null result");
        Assert.True(nullGuard < clear,
            "the null guard must precede SetBusy(false) so a pending fetch keeps its indicator");
    }

    [Fact]
    public void SuccessfulMergeInvalidatesSuggestionsForBothChangedEndpoints()
    {
        var code = File.ReadAllText(SheetCode());

        Assert.Contains("other.SourcePersonId == vm.SourcePersonId", code, StringComparison.Ordinal);
        Assert.Contains("other.DestinationPersonId == vm.SourcePersonId", code, StringComparison.Ordinal);
        Assert.Contains("other.SourcePersonId == vm.DestinationPersonId", code, StringComparison.Ordinal);
        Assert.Contains("other.DestinationPersonId == vm.DestinationPersonId", code, StringComparison.Ordinal);
    }

    private static int CountOf(string haystack, string needle)
    {
        int n = 0, i = 0;
        while ((i = haystack.IndexOf(needle, i, StringComparison.Ordinal)) >= 0) { n++; i += needle.Length; }
        return n;
    }

    private static string FindRepoRoot()
    {
        var dir = AppContext.BaseDirectory;
        while (dir is not null)
        {
            if (Directory.Exists(Path.Combine(dir, "platforms")) &&
                Directory.Exists(Path.Combine(dir, "shared")))
            {
                return dir;
            }
            dir = Path.GetDirectoryName(dir.TrimEnd(Path.DirectorySeparatorChar));
        }
        throw new DirectoryNotFoundException("repo root not found from " + AppContext.BaseDirectory);
    }
}
