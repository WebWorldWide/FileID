using Xunit;

namespace FileID.App.Tests;

public sealed class WinUiDrillFixContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    private static string App(params string[] parts)
        => File.ReadAllText(Path.Combine([RepoRoot, "platforms", "windows", "src", "FileID.App", .. parts]));

    [Fact]
    public void DescribeFilesApply_RestoresButtonsWhenTheEngineCallThrows()
    {
        var view = App("Views", "DeepAnalyze", "DeepAnalyzeView.xaml.cs");
        var start = view.IndexOf("private async System.Threading.Tasks.Task RunApplyAsync", StringComparison.Ordinal);
        var end = view.IndexOf("private void SetApplyBusy", start, StringComparison.Ordinal);
        var body = view[start..end];
        var caught = body.IndexOf("catch (Exception ex)", StringComparison.Ordinal);
        Assert.True(caught > 0);
        Assert.True(body.IndexOf("SetApplyBusy(false, \"Couldn't add tags", caught, StringComparison.Ordinal) > caught);
    }

    [Fact]
    public void CleanupRefreshButton_VisibleLabelMatchesItsAccessibleNameAndTheAlerts()
    {
        var xaml = App("Views", "Cleanup", "CleanupView.xaml");
        Assert.Contains("AutomationProperties.Name=\"Check again\"", xaml, StringComparison.Ordinal);
        Assert.Contains("<TextBlock Text=\"Check again\"", xaml, StringComparison.Ordinal);
        Assert.DoesNotContain("<TextBlock Text=\"Refresh\"", xaml, StringComparison.Ordinal);
    }

    [Fact]
    public void PreviewRename_PushesUndoRefreshesPathAndReportsFailure()
    {
        var sheet = App("Views", "Library", "FilePreviewSheet.xaml.cs");
        var start = sheet.IndexOf("private async void OnApplyRenameClicked", StringComparison.Ordinal);
        var end = sheet.IndexOf("private void OnDismissRenameClicked", start, StringComparison.Ordinal);
        var body = sheet[start..end];
        Assert.Contains("ConfirmsExactSuccess(result, new[] { FileId })", body, StringComparison.Ordinal);
        Assert.Contains("UndoStack.Instance.Push(", body, StringComparison.Ordinal);
        Assert.Contains("FilePath = newPath;", body, StringComparison.Ordinal);
        Assert.Contains("FileNameText.Text = name!;", body, StringComparison.Ordinal);
        Assert.Contains("ShowTagStatus(\"Couldn't rename", body, StringComparison.Ordinal);
    }

    [Fact]
    public void BulkTagSheet_HasNoDeadCtrlEnterHandler()
    {
        Assert.DoesNotContain("BulkTagPrimaryButton", App("Views", "Library", "BulkTagSheet.xaml.cs"), StringComparison.Ordinal);
        Assert.DoesNotContain("OnTagsKeyDown", App("Views", "Library", "BulkTagSheet.xaml"), StringComparison.Ordinal);
    }

    [Fact]
    public void ModelCatalogBrowser_RerunsARefreshRequestedWhileOneIsInFlight()
    {
        var browser = App("Views", "ModelBrowser", "ModelCatalogBrowser.cs");
        Assert.Contains("Volatile.Write(ref _refreshQueued, 1);", browser, StringComparison.Ordinal);
        Assert.Contains("Interlocked.Exchange(ref _refreshQueued, 0) != 0) Refresh();", browser, StringComparison.Ordinal);
    }

    [Fact]
    public void RestructureUndoStatus_KnowsWhenTheUndoWasOfShortcuts()
    {
        var client = App("ViewModels", "EngineClient.cs");
        Assert.Contains("LastRestructureResultWasShortcutUndo = UndoRestructureInFlight && UndoRestructureShortcutToken is not null;", client, StringComparison.Ordinal);
        Assert.Contains("FormatUndoCompletion(r, EngineClient.Instance.LastRestructureResultWasShortcutUndo)", App("Views", "Restructure", "RestructureView.xaml.cs"), StringComparison.Ordinal);
    }

    [Fact]
    public void PersonDetail_NamingAnAlreadyUnknownPersonRenamesThemInsteadOfMarkingUnknown()
    {
        var sheet = App("Views", "People", "PersonDetailSheet.xaml.cs");
        Assert.Contains("_loadedUnknown = result.IsUnknown;", sheet, StringComparison.Ordinal);
        Assert.Contains("IsUnknownCheckBox.IsChecked == true && !(_loadedUnknown && HasTypedName())", sheet, StringComparison.Ordinal);
    }

    [Fact]
    public void SettingsRecentScans_DisposesItsSqliteConnection()
    {
        var settings = App("Views", "Settings", "SettingsView.xaml.cs");
        var start = settings.IndexOf("PopulateRecentScansAsync()", StringComparison.Ordinal);
        Assert.True(settings.IndexOf("using var conn = new Microsoft.Data.Sqlite.SqliteConnection(", start, StringComparison.Ordinal) > start);
    }

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
