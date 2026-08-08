using Xunit;

namespace FileID.App.Tests;

public sealed class UiInteractionSafetyContractTests
{
    [Fact]
    public void LibraryKeyboardCursorCanOpenCurrentTileContextMenu()
    {
        var source = ReadSource("Views", "Library", "LibraryView.xaml.cs");

        Assert.Contains("e.Key == VirtualKey.Application", source, StringComparison.Ordinal);
        Assert.Contains("e.Key == VirtualKey.F10 && shift", source, StringComparison.Ordinal);
        Assert.Contains("flyout.ShowAt(target)", source, StringComparison.Ordinal);
    }

    [Fact]
    public void LibraryTrashRejectsReentryUntilResultFinishes()
    {
        var source = ReadSource("Views", "Library", "LibraryView.xaml.cs");
        var body = Between(
            source,
            "private async void OnTrashSelectedClicked",
            "// Dismissible alert");

        Assert.Contains("CompareExchange(ref _trashInFlight", body, StringComparison.Ordinal);
        Assert.Contains("finally", body, StringComparison.Ordinal);
        Assert.Contains("Exchange(ref _trashInFlight, 0)", body, StringComparison.Ordinal);
    }

    [Fact]
    public void PeopleCardsExposeKeyboardDetailAndContextPaths()
    {
        var xaml = ReadSource("Views", "People", "PeopleView.xaml");
        var detailXaml = ReadSource("Views", "People", "PersonDetailSheet.xaml");
        var source = ReadSource("Views", "People", "PeopleView.xaml.cs");

        Assert.Contains("IsTabStop=\"True\"", xaml, StringComparison.Ordinal);
        Assert.Contains("KeyDown=\"OnClusterKeyDown\"", xaml, StringComparison.Ordinal);
        Assert.Contains("Click=\"OnClusterEditNameClicked\"", xaml, StringComparison.Ordinal);
        Assert.Contains("AutomationProperties.Name=\"Edit person name\"", xaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"NameFieldsPanel\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"TitleBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"FirstBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"MiddleBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"LastBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"SuffixBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"IsUnknownCheckBox\"", detailXaml, StringComparison.Ordinal);
        Assert.Contains("OnClusterEditNameClicked", source, StringComparison.Ordinal);
        Assert.Contains("await OpenDetailSheetAsync(cluster)", source, StringComparison.Ordinal);
    }

    [Fact]
    public void PersonDropMergeSharesTheBulkOperationGate()
    {
        var source = ReadSource("Views", "People", "PeopleView.xaml.cs");
        var body = Between(
            source,
            "private async void OnClusterDrop",
            "// ─── FEAT-CRIT-1");

        Assert.Contains("CompareExchange(ref _bulkOpInFlight", body, StringComparison.Ordinal);
        Assert.Contains("finally", body, StringComparison.Ordinal);
        Assert.Contains("Exchange(ref _bulkOpInFlight, 0)", body, StringComparison.Ordinal);
    }

    [Fact]
    public void RestructureRepeatersPrepareDataContextForInteractiveHandlers()
    {
        var viewXaml = ReadSource("Views", "Restructure", "RestructureView.xaml");
        var viewCode = ReadSource("Views", "Restructure", "RestructureView.xaml.cs");
        var sheetXaml = ReadSource("Views", "Restructure", "DrillDownSheet.xaml");
        var sheetCode = ReadSource("Views", "Restructure", "DrillDownSheet.xaml.cs");

        Assert.Contains(
            "ElementPrepared=\"OnRecommendationElementPrepared\"",
            viewXaml,
            StringComparison.Ordinal);
        Assert.Contains(
            "ElementPrepared=\"OnFileElementPrepared\"",
            viewXaml,
            StringComparison.Ordinal);
        Assert.Contains(
            "ResolveRepeaterItem<RestructureRecommendationVm>(sender.ItemsSource, args.Index)",
            viewCode,
            StringComparison.Ordinal);
        Assert.Contains(
            "ResolveRepeaterItem<RestructureFileRowVm>(sender.ItemsSource, args.Index)",
            viewCode,
            StringComparison.Ordinal);
        Assert.Contains(
            "ElementPrepared=\"OnSelectionElementPrepared\"",
            sheetXaml,
            StringComparison.Ordinal);
        Assert.Contains(
            "RestructureView.ResolveRepeaterItem<RestructureFileRowVm>",
            sheetCode,
            StringComparison.Ordinal);
    }

    private static string Between(string source, string startMarker, string endMarker)
    {
        var start = source.IndexOf(startMarker, StringComparison.Ordinal);
        var end = source.IndexOf(endMarker, start, StringComparison.Ordinal);
        Assert.True(start >= 0 && end > start);
        return source[start..end];
    }

    private static string ReadSource(params string[] parts) =>
        File.ReadAllText(Path.Combine(
            FindRepoRoot(),
            "platforms",
            "windows",
            "src",
            "FileID.App",
            Path.Combine(parts)));

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
