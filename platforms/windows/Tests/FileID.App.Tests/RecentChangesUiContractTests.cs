using System.Globalization;
using System.Xml.Linq;
using Xunit;

namespace FileID.App.Tests;

public sealed class RecentChangesUiContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void SheetFitsInsideTheContentDialogAndStretchesRows()
    {
        var document = XDocument.Load(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "SessionChangesSheet.xaml"));
        var root = Assert.IsType<XElement>(document.Root);
        var minWidth = double.Parse(root.Attribute("MinWidth")?.Value ?? "0", CultureInfo.InvariantCulture);
        var maxWidth = double.Parse(root.Attribute("MaxWidth")?.Value ?? "Infinity", CultureInfo.InvariantCulture);
        var xaml = root.GetDefaultNamespace();
        var rowsHost = Assert.Single(
            root.Descendants(xaml + "ItemsControl"),
            element => element.Attribute(XName.Get("Name", "http://schemas.microsoft.com/winfx/2006/xaml"))?.Value == "RowsHost");

        Assert.InRange(minWidth, 0, 480);
        Assert.InRange(maxWidth, minWidth, 480);
        Assert.Equal("Stretch", rowsHost.Attribute("HorizontalContentAlignment")?.Value);
    }

    [Fact]
    public void ControlsResubscribeAfterReloadAndKeepBrushesOnTheUiThread()
    {
        var sidebar = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarRecentChanges.xaml.cs"));
        var sheet = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "SessionChangesSheet.xaml.cs"));

        AssertReloadableSubscription(sidebar);
        AssertReloadableSubscription(sheet);
        Assert.DoesNotContain("static readonly SolidColorBrush", sheet, StringComparison.Ordinal);
    }

    [Fact]
    public void PendingBadgeAndCloseGateIncludeFailedAndInFlightUndo()
    {
        var sidebar = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarRecentChanges.xaml.cs"));
        var sheet = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "SessionChangesSheet.xaml.cs"));
        var window = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "MainWindow.xaml.cs"));

        Assert.Contains("ChangeLog.Instance.PendingCount", sidebar, StringComparison.Ordinal);
        Assert.DoesNotContain("ChangeLog.Instance.UndoableCount", sidebar, StringComparison.Ordinal);
        Assert.Contains("ChangeLog.Instance.PendingCount", window, StringComparison.Ordinal);
        Assert.Contains("ChangeStatus.Undoing => \"Undoing…\"", sheet, StringComparison.Ordinal);
        Assert.Contains("IsEnabled = !ChangeLog.Instance.IsUndoInFlight", sheet, StringComparison.Ordinal);
        Assert.Contains("entry.Status == ChangeStatus.Undoing", sheet, StringComparison.Ordinal);
        Assert.Contains("ProgressRing", sheet, StringComparison.Ordinal);
    }

    [Fact]
    public void UndoRowDirectlyRefreshesItsVisibleDialogState()
    {
        var sheet = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "SessionChangesSheet.xaml.cs"));
        var undo = sheet.IndexOf("ChangeLog.Instance.UndoAsync(entry)", StringComparison.Ordinal);
        var rebuild = sheet.IndexOf("DispatcherQueue.HasThreadAccess", undo, StringComparison.Ordinal);

        Assert.True(undo >= 0 && rebuild > undo);
        Assert.Contains("DispatcherQueue.TryEnqueue(Rebuild)", sheet[rebuild..], StringComparison.Ordinal);
    }

    [Fact]
    public void PeopleUndoWaitsForAReadyEngineAndRequiresItsTerminalResult()
    {
        var source = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "People", "PeopleView.xaml.cs"));
        var method = source.IndexOf("internal static void PushMergeUndo", StringComparison.Ordinal);
        Assert.True(method >= 0, "PeopleView.PushMergeUndo must remain present.");
        var body = source[method..];

        var ready = body.IndexOf("WaitForReadyAsync", StringComparison.Ordinal);
        var wait = body.IndexOf("WaitForBulkActionResultAsync", StringComparison.Ordinal);

        Assert.True(ready >= 0 && wait > ready, "People undo must wait for a usable engine before sending revertMerge.");
        Assert.Contains("r.Failed == 0 && r.Succeeded > 0", body, StringComparison.Ordinal);
        Assert.DoesNotContain("return true;", body, StringComparison.Ordinal);
    }

    [Fact]
    public void TrashUndoReturnsTheEngineConfirmedRestoreResult()
    {
        AssertRestoreResultIsReturned(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Library", "LibraryView.xaml.cs"));
        AssertRestoreResultIsReturned(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Cleanup", "CleanupView.xaml.cs"));
    }

    private static void AssertRestoreResultIsReturned(string path)
    {
        var source = File.ReadAllText(path);
        var restore = source.IndexOf("RestoreFromTrashAsync", StringComparison.Ordinal);
        Assert.True(restore >= 0, $"{Path.GetFileName(path)} must keep its trash-undo closure.");
        var start = Math.Max(0, restore - 100);
        var window = source[start..Math.Min(source.Length, restore + 100)];

        Assert.Contains("return await", window, StringComparison.Ordinal);
        Assert.DoesNotContain("return true", window, StringComparison.Ordinal);
    }

    private static void AssertReloadableSubscription(string source)
    {
        Assert.Contains("Loaded += OnLoaded;", source, StringComparison.Ordinal);
        Assert.Contains("Unloaded += OnUnloaded;", source, StringComparison.Ordinal);

        var loaded = source.IndexOf("private void OnLoaded", StringComparison.Ordinal);
        var unloaded = source.IndexOf("private void OnUnloaded", StringComparison.Ordinal);
        var subscribe = source.IndexOf("ChangeLog.Instance.Changed += OnChangeLogChanged;", loaded, StringComparison.Ordinal);
        var unsubscribe = source.IndexOf("ChangeLog.Instance.Changed -= OnChangeLogChanged;", unloaded, StringComparison.Ordinal);

        Assert.True(loaded >= 0 && subscribe > loaded, "The control must subscribe when it loads.");
        Assert.True(unloaded >= 0 && unsubscribe > unloaded, "The control must unsubscribe when it unloads.");
        Assert.Contains("_unloaded = false;", source[loaded..unloaded], StringComparison.Ordinal);
    }

    private static string PathInRepo(params string[] parts)
        => Path.Combine([RepoRoot, .. parts]);

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
