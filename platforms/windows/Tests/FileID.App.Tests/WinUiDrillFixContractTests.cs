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

    [Fact]
    public void CloseFolder_CancelsARunningScanBeforeClearingTheFolder()
    {
        var header = App("Views", "Sidebar", "SidebarFolderHeader.xaml.cs");
        var start = header.IndexOf("private async void OnClearClicked", StringComparison.Ordinal);
        var cancel = header.IndexOf("engine.CancelScanAsync()", start, StringComparison.Ordinal);
        var clear = header.IndexOf("AppViewModel.Instance.FolderPath = null;", start, StringComparison.Ordinal);
        Assert.True(start >= 0 && cancel > start && clear > cancel);
    }

    [Fact]
    public void LocationsCard_RefreshesRowsWhenAScanEnds()
    {
        var card = App("Views", "Settings", "LocationsCard.cs");
        Assert.Contains("_engine.PropertyChanged += OnEngineChanged;", card, StringComparison.Ordinal);
        Assert.Contains("_engine.PropertyChanged -= OnEngineChanged;", card, StringComparison.Ordinal);
        Assert.Contains("ScanPhase.Completed or ScanPhase.Cancelled or ScanPhase.Failed", card, StringComparison.Ordinal);
    }

    [Fact]
    public void OpenEngineLog_RevealsTheDatedLogInsteadOfLaunchingIt()
    {
        var control = App("Views", "Sidebar", "SidebarProcessingControl.xaml.cs");
        var start = control.IndexOf("private void OnOpenLogClicked", StringComparison.Ordinal);
        var end = control.IndexOf("OnOpenLogClicked threw", start, StringComparison.Ordinal);
        var body = control[start..end];
        Assert.Contains("AppPaths.LogsDir", body, StringComparison.Ordinal);
        Assert.Contains("SafeOpen.Reveal(newest)", body, StringComparison.Ordinal);
        Assert.DoesNotContain("FileName = target", body, StringComparison.Ordinal);
    }

    [Fact]
    public void TakesGoalRanking_FlagIsClearedWhenARunEndsAndNotStartedWhileBusy()
    {
        var takes = App("Views", "Workbenches", "TakesWorkbench.cs");
        var update = takes.IndexOf("protected override void UpdateControls()", StringComparison.Ordinal);
        Assert.True(takes.IndexOf("if (!Busy && _goalRanking)", update, StringComparison.Ordinal) > update);
        Assert.Contains("if (_goalRanking || Busy ||", App("Views", "Workbenches", "TakesWorkbench.Goal.cs"), StringComparison.Ordinal);
    }

    [Fact]
    public void CatalogWorkbench_ReplaysAResultSelectionMadeWhileBusy()
    {
        var catalog = App("Views", "Workbenches", "CatalogWorkbench.cs");
        Assert.Contains("if (Busy) _detailRefreshPending = true;", catalog, StringComparison.Ordinal);
        Assert.Contains("if (_detailRefreshPending && !Busy)", catalog, StringComparison.Ordinal);
    }

    [Fact]
    public void ChatSend_UnchecksResultTilesBeforeClearingTheChosenSet()
    {
        var selection = App("Views", "Workbenches", "ChatWorkbench.Selection.cs");
        var start = selection.IndexOf("private void BeginScopedSend", StringComparison.Ordinal);
        var uncheck = selection.IndexOf("tile.IsChecked = false;", start, StringComparison.Ordinal);
        var clear = selection.IndexOf("_chosen.Clear();", start, StringComparison.Ordinal);
        Assert.True(start >= 0 && uncheck > start && clear > uncheck);
    }

    [Fact]
    public void TakesScoring_ShowsFailureOnExceptionsAndKeepsGroupListInSyncWhileBusy()
    {
        var takes = App("Views", "Workbenches", "TakesWorkbench.cs");
        var score = takes.IndexOf("private async Task ScoreAsync", StringComparison.Ordinal);
        var caught = takes.IndexOf("catch (Exception ex) when (!ct.IsCancellationRequested)", score, StringComparison.Ordinal);
        Assert.True(caught > score);
        Assert.True(takes.IndexOf("ShowFailure(group, ex is TimeoutException", caught, StringComparison.Ordinal) > caught);
        Assert.Contains("RestoreGroupSelection();", takes, StringComparison.Ordinal);
        Assert.Contains("&& !_scoreFailed) return;", takes, StringComparison.Ordinal);
    }

    [Fact]
    public void TakesTrash_ActsOnTheSnapshotTheUserConfirmedAndTellsThemWhenItCannot()
    {
        var takes = App("Views", "Workbenches", "TakesWorkbench.cs");
        Assert.Contains("Run(ct => TrashAsync(group, confirmedQualities, confirmedKeep, ct));", takes, StringComparison.Ordinal);
        Assert.Contains("This group changed while you were confirming", takes, StringComparison.Ordinal);
        Assert.Contains("FileID is busy for a moment. Nothing was moved", takes, StringComparison.Ordinal);
    }

    [Fact]
    public void FileTools_ReleasesThePreviewedVideoWhenThePanelCloses()
    {
        var tools = App("Views", "Workbenches", "ToolsWorkbench.cs");
        var start = tools.IndexOf("protected override void OnClosed()", StringComparison.Ordinal);
        var end = tools.IndexOf("private void OnToolFilesRequested", start, StringComparison.Ordinal);
        var body = tools[start..end];
        Assert.Contains("previous?.Dispose();", body, StringComparison.Ordinal);
        Assert.Contains("_playerPath = null;", body, StringComparison.Ordinal);
        Assert.DoesNotContain("_player?.Pause();", body, StringComparison.Ordinal);
    }

    [Fact]
    public void LavaLamp_StartsWithTheWaxCentredSoAnOffLampShowsTheStaticGlow()
    {
        var lamp = File.ReadAllText(Path.Combine(RepoRoot, "platforms", "windows", "src", "FileID.Theme", "Motion", "LavaLampBackground.cs"));
        var build = lamp.IndexOf("private void BuildVisualTree()", StringComparison.Ordinal);
        var center = lamp.IndexOf("CenterVisual(_goldVisual, _root.Size);", build, StringComparison.Ordinal);
        var insert = lamp.IndexOf("_root.Children.InsertAtTop(_goldVisual);", build, StringComparison.Ordinal);
        Assert.True(build >= 0 && center > build && insert > center);
    }

    [Fact]
    public void ReadOnlyRefusal_IsShownWithTheEngineSentenceInOrganizeFoldersAndChat()
    {
        var view = App("Views", "Restructure", "RestructureView.xaml.cs");
        Assert.Contains("err.Kind != \"location_read_only\"", view, StringComparison.Ordinal);
        Assert.Contains("ShowAlertAsync(\"Nothing was moved\", sentence)", view, StringComparison.Ordinal);
        Assert.Contains("_ = ShowFailedApplyAlertAsync(r);", view, StringComparison.Ordinal);
        var chat = App("ViewModels", "EngineClient.ChatActions.cs");
        Assert.Contains("\"db_unavailable\", \"location_read_only\"", chat, StringComparison.Ordinal);
        Assert.Contains("deferResult: result => result.Applied == 0 && result.Failed > 0 && !result.Cancelled", chat, StringComparison.Ordinal);
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
