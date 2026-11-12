using System.Globalization;
using System.Text.Json;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatActionPolicyTests
{
    private static ChatAction Action(string kind, bool? confirm = null, string id = "r:1") =>
        new(id, kind, "Do it", "What happens", confirm ?? ChatActionPolicy.Confirming.Contains(kind), "parser");

    private static ChatAction Trash(IReadOnlyList<long> victims, IReadOnlyList<long> keepers) =>
        Action("moveDuplicatesToTrash") with { FileIDs = victims, KeeperFileIDs = keepers, FileCount = victims.Count, ByteCount = 4_200_000 };

    [Fact]
    public void KindsPagesAndSettingsMatchTheSchema()
    {
        using var schema = JsonDocument.Parse(File.ReadAllText(Path.Combine(FindRepoRoot(), "shared", "ipc-schema", "ipc.schema.json")));
        var defs = schema.RootElement.GetProperty("$defs");
        var action = defs.GetProperty("ChatAction").GetProperty("properties");
        Assert.Equal(Enum(action.GetProperty("kind")), ChatActionPolicy.Kinds.Order());
        Assert.Equal(Enum(action.GetProperty("setting")), ChatActionPolicy.Settings.Order());
        Assert.Equal(Enum(defs.GetProperty("ChatPage")), ChatActionPolicy.Pages.Order());
        Assert.Equal(17, ChatActionPolicy.Kinds.Count);
        Assert.True(ChatActionPolicy.Confirming.IsSubsetOf(ChatActionPolicy.Kinds));

        static IEnumerable<string> Enum(JsonElement element) =>
            element.GetProperty("enum").EnumerateArray().Select(value => value.GetString()!).Order();
    }

    [Theory]
    [InlineData("moveDuplicatesToTrash")]
    [InlineData("applyFolderLayout")]
    [InlineData("undoLastMove")]
    [InlineData("renameToSuggested")]
    [InlineData("namePerson")]
    [InlineData("mergePeople")]
    public void ChangingKindsNeverRunWithoutConfirmation(string kind)
    {
        Assert.Contains(kind, ChatActionPolicy.Confirming);
        Assert.False(ChatActionPolicy.IsRunnable(Action(kind, confirm: false)));
    }

    [Theory]
    [InlineData("rescan")]
    [InlineData("describeFiles")]
    [InlineData("stopJob")]
    [InlineData("findDuplicates")]
    [InlineData("suggestFolderLayout")]
    [InlineData("openPage")]
    [InlineData("search")]
    [InlineData("findMoments")]
    public void ReadOnlyKindsRunDirectlyAndRejectAWrongFlag(string kind)
    {
        Assert.True(ChatActionPolicy.IsRunnable(Action(kind)));
        Assert.False(ChatActionPolicy.IsRunnable(Action(kind, confirm: true)));
    }

    [Fact]
    public void UnknownOrMalformedActionsAreDropped()
    {
        Assert.False(ChatActionPolicy.IsRunnable(Action("deleteEverything", confirm: true)));
        Assert.False(ChatActionPolicy.IsRunnable(Action("openPage") with { Page = "terminal" }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("changeSetting") with { Setting = "telemetry", Value = true }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("changeSetting") with { Setting = "cleanupAutoTag" }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("stopJob") with { Job = "everything" }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("scanFolder")));
        Assert.False(ChatActionPolicy.IsRunnable(Action("namePerson") with { PersonID = 4 }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("mergePeople") with { PersonID = 4, PersonIDs = new long[] { 4 } }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("renameToSuggested") with { Renames = new[] { new ChatRename(3, "..\\escape.jpg") } }));
        Assert.False(ChatActionPolicy.IsRunnable(Action("exportFiles")));
        Assert.True(ChatActionPolicy.IsRunnable(Action("namePerson") with { PersonID = 4, Name = "Maya Lopez" }));
        Assert.True(ChatActionPolicy.IsRunnable(Action("mergePeople") with { PersonID = 4, PersonIDs = new long[] { 5, 6 } }));
        Assert.True(ChatActionPolicy.IsRunnable(Action("renameToSuggested") with { Renames = new[] { new ChatRename(3, "Beach day.jpg") } }));
        Assert.True(ChatActionPolicy.IsRunnable(Action("scanFolder") with { Path = "C:\\Photos" }));
    }

    [Fact]
    public void TrashNeedsOneKeeperPerCopyAndNeverTrashesAKeeper()
    {
        Assert.True(ChatActionPolicy.IsRunnable(Trash(new long[] { 2, 3 }, new long[] { 1, 1 })));
        Assert.False(ChatActionPolicy.IsRunnable(Trash(new long[] { 2, 3 }, new long[] { 1 })));
        Assert.False(ChatActionPolicy.IsRunnable(Trash(new long[] { 2 }, new long[] { 2 })));
        Assert.False(ChatActionPolicy.IsRunnable(Trash(new long[] { 2, 3 }, new long[] { 3, 1 })));
        Assert.False(ChatActionPolicy.IsRunnable(Trash(new long[] { 2, 2 }, new long[] { 1, 1 })));
        Assert.False(ChatActionPolicy.IsRunnable(Trash(Array.Empty<long>(), Array.Empty<long>())));
        Assert.False(ChatActionPolicy.IsRunnable(Action("moveDuplicatesToTrash") with { FileIDs = new long[] { 2 } }));
    }

    [Fact]
    public void RunnableFiltersAndKeepsTheFirstOfEachID()
    {
        var actions = new[] { Action("rescan", id: "a"), Action("rescan", id: "a"), Action("moveDuplicatesToTrash", confirm: false, id: "b"), Action("search", id: "c") };
        Assert.Equal("a,c", string.Join(",", ChatActionPolicy.Runnable(actions).Select(action => action.Id)));
        Assert.Empty(ChatActionPolicy.Runnable(null));
    }

    [Fact]
    public void LabelsAndAccentsFollowThePortNote()
    {
        Assert.Equal("Needs your OK", ChatActionPolicy.Label(Action("namePerson")));
        Assert.Equal("Suggested", ChatActionPolicy.Label(Action("search")));
        Assert.Equal(ChatActionAccent.Danger, ChatActionPolicy.Accent(Action("moveDuplicatesToTrash")));
        Assert.Equal(ChatActionAccent.Gold, ChatActionPolicy.Accent(Action("renameToSuggested")));
        Assert.Equal(ChatActionAccent.Info, ChatActionPolicy.Accent(Action("findMoments")));
        Assert.True(ChatActionPolicy.CanStop("describeFiles"));
        Assert.False(ChatActionPolicy.CanStop("moveDuplicatesToTrash"));
    }

    [Fact]
    public void ExamplesShowFileNamesUpToThreeAndCountTheRest()
    {
        var action = Trash(new long[] { 2, 3, 4, 5, 6 }, new long[] { 1, 1, 1, 1, 1 }) with
        {
            Examples = new[] { "C:\\Photos\\IMG_1 copy.jpg", "/Users/me/Pictures/IMG_2 copy.jpg", "IMG_3 copy.jpg", "IMG_4 copy.jpg" },
        };
        Assert.Equal("IMG_1 copy.jpg|IMG_2 copy.jpg|IMG_3 copy.jpg", string.Join("|", ChatActionPolicy.Examples(action, null)));
        Assert.Equal(2, ChatActionPolicy.MoreCount(action, null));
        Assert.Equal(5, ChatActionPolicy.FileCount(action, null));
        Assert.Equal("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ", ChatActionPolicy.MiddleTruncate("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        var truncated = ChatActionPolicy.MiddleTruncate(new string('a', 40) + new string('b', 40), 21);
        Assert.Equal(21, truncated.Length);
        Assert.Equal(new string('a', 10) + "…" + new string('b', 10), truncated);
    }

    [Fact]
    public void FolderLayoutAppliesOnlyTheAutoTierOfTheMatchingPlan()
    {
        var plan = new RestructurePlan("C:\\Photos", new[]
        {
            new RestructureMove(1, "C:\\Photos\\a.jpg", "C:\\Photos\\2024\\Beach\\a.jpg", "Events", Confidence: "auto"),
            new RestructureMove(2, "C:\\Photos\\b.jpg", "C:\\Photos\\2024\\b.jpg", "Events", Confidence: "review"),
            new RestructureMove(3, "C:\\Photos\\c.jpg", "C:\\Photos\\c2.jpg", "Events", Confidence: "auto"),
        }, Array.Empty<RestructureCategoryCount>());
        var apply = Action("applyFolderLayout") with { Path = "C:\\Photos\\" };
        var summary = ChatActionPolicy.Plan(plan, apply);
        Assert.NotNull(summary);
        Assert.False(summary.TooLarge);
        Assert.Equal(new long[] { 1, 3 }, summary.Moves.Select(move => move.FileId));
        Assert.Equal("a.jpg → 2024\\Beach|c.jpg → top folder", string.Join("|", summary.Examples));
        Assert.Equal("Move 2 files", ChatActionPolicy.PrimaryTitle(apply, summary));
        Assert.Contains("Moves 2 files in Photos", ChatActionPolicy.Detail(apply, summary, canUndoMove: false), StringComparison.Ordinal);
        Assert.False(ChatActionPolicy.IsUnavailable(apply, summary, canUndoMove: false));

        Assert.Null(ChatActionPolicy.Plan(plan, apply with { Path = "D:\\Other" }));
        Assert.Null(ChatActionPolicy.Plan(null, apply));
        Assert.True(ChatActionPolicy.IsUnavailable(apply, null, canUndoMove: true));
        Assert.Equal("Do it", ChatActionPolicy.PrimaryTitle(apply, null));
        Assert.Contains("no suggested layout", ChatActionPolicy.Detail(apply, null, canUndoMove: false), StringComparison.Ordinal);
    }

    [Fact]
    public void APagedPlanIsNotAppliedFromChat()
    {
        var plan = new RestructurePlan("C:\\Photos", Array.Empty<RestructureMove>(), Array.Empty<RestructureCategoryCount>(),
            TotalMoves: 90_000, ConfidenceCounts: new RestructureConfidenceCounts(80_000, 10_000, 0, 0), Truncated: true, PlanId: "p1");
        var apply = Action("applyFolderLayout");
        var summary = ChatActionPolicy.Plan(plan, apply);
        Assert.NotNull(summary);
        Assert.True(summary.TooLarge);
        Assert.Empty(summary.Moves);
        Assert.True(ChatActionPolicy.IsUnavailable(apply, summary, canUndoMove: false));
        Assert.Contains("more moves than Chat can apply", ChatActionPolicy.Detail(apply, summary, canUndoMove: false), StringComparison.Ordinal);
    }

    [Fact]
    public void UndoLastMoveAndSettingsAreDisabledWhenNothingCanRun()
    {
        Assert.True(ChatActionPolicy.IsUnavailable(Action("undoLastMove"), null, canUndoMove: false));
        Assert.False(ChatActionPolicy.IsUnavailable(Action("undoLastMove"), null, canUndoMove: true));
        Assert.Equal("There's no applied layout to undo.", ChatActionPolicy.Detail(Action("undoLastMove"), null, canUndoMove: false));
        Assert.False(ChatActionPolicy.IsUnavailable(Action("changeSetting") with { Setting = "cleanupAutoTag", Value = false }, null, false));
        Assert.False(ChatActionPolicy.IsUnavailable(Action("changeSetting") with { Setting = "backgroundMotion", Value = false }, null, false));
        Assert.False(ChatActionPolicy.IsUnavailable(Action("changeSetting") with { Setting = "chatSummaries", Value = true }, null, false));
        Assert.True(ChatActionPolicy.IsUnavailable(Action("changeSetting") with { Setting = "telemetry", Value = true }, null, false));
    }

    [Fact]
    public void ChatSettingsReadAndWriteTheirAppSettings()
    {
        var settings = new AppSettings();
        Assert.True(ChatActionPolicy.SettingValue(settings, "backgroundMotion"));
        Assert.True(ChatActionPolicy.SettingValue(settings, "chatSummaries"));
        ChatActionPolicy.ApplySetting(settings, "chatSummaries", false);
        Assert.False(settings.ChatUseModel);
        ChatActionPolicy.ApplySetting(settings, "cleanupAutoTag", false);
        Assert.False(ChatActionPolicy.SettingValue(settings, "cleanupAutoTag"));
        settings.LavaMode = AppSettings.LavaModeOff;
        Assert.False(ChatActionPolicy.SettingValue(settings, "backgroundMotion"));
    }

    [Fact]
    public void NamesSplitIntoFirstAndLast()
    {
        Assert.Equal(("Maya", "de la Cruz"), ChatActionPolicy.PersonName("  Maya  de la Cruz "));
        Assert.Equal(("Kyle", (string?)null), ChatActionPolicy.PersonName("Kyle"));
        Assert.Equal(((string?)null, (string?)null), ChatActionPolicy.PersonName("   "));
    }

    [Fact]
    public void RenamesApplyOnlyWhileTheSuggestionIsUnchanged()
    {
        Assert.True(ChatActionPolicy.SuggestionStillMatches("Beach day", "C:\\Photos\\IMG_1.JPG", "Beach day.JPG"));
        Assert.True(ChatActionPolicy.SuggestionStillMatches(" Notes ", "/docs/README", "Notes"));
        Assert.False(ChatActionPolicy.SuggestionStillMatches("Beach night", "C:\\Photos\\IMG_1.JPG", "Beach day.JPG"));
        Assert.False(ChatActionPolicy.SuggestionStillMatches(null, "C:\\Photos\\IMG_1.JPG", "Beach day.JPG"));
    }

    [Fact]
    public void ContextCarriesTheLibrarySelectionAndFolder()
    {
        var context = ChatActionPolicy.Context(Enumerable.Range(1, 12_000).Select(i => (long)i).ToArray(), "C:\\Photos");
        Assert.Equal("chat", context.Page);
        Assert.Equal(ChatActionPolicy.MaxContextFiles, context.FileIDs!.Count);
        Assert.Equal("C:\\Photos", context.Folder);
        Assert.Equal("C:\\Photos", context.LibraryRoot);
        var empty = ChatActionPolicy.Context(Array.Empty<long>(), " ");
        Assert.Null(empty.FileIDs);
        Assert.Null(empty.LibraryRoot);
        var json = JsonSerializer.Serialize(empty, IpcCoder.Options);
        Assert.Equal("{\"page\":\"chat\"}", json);
    }

    [Theory]
    [InlineData("library", "library")]
    [InlineData("people", "people")]
    [InlineData("cleanup", "cleanup")]
    [InlineData("deepAnalyze", "deepanalyze")]
    [InlineData("restructure", "restructure")]
    [InlineData("settings", "settings")]
    [InlineData("search", "moments")]
    [InlineData("tools", "tools")]
    [InlineData("chat", "chat")]
    public void EveryChatPageOpensARealPage(string page, string tab)
    {
        Assert.Equal(tab, ChatActionPolicy.TabId(page));
        Assert.Equal(tab, SidebarTab.ById(tab).Id);
        Assert.NotEqual("", ChatActionPolicy.PageTitle(page));
    }

    [Fact]
    public void ResultLinesReadPlainly()
    {
        var culture = CultureInfo.CurrentCulture;
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
        try { AssertResultLines(); }
        finally { CultureInfo.CurrentCulture = culture; }
    }

    private static void AssertResultLines()
    {
        Assert.Equal("Moved 3 copies to the Recycle Bin · freed 4.2 MB · 1 skipped because the file changed since the scan.",
            ChatActionPolicy.TrashResult(3, 4_200_000, 1, 0));
        Assert.Equal("Moved 1 copy to the Recycle Bin · freed 1 KB · 2 couldn't be moved.", ChatActionPolicy.TrashResult(1, 1000, 0, 2));
        Assert.Equal("Renamed 4 files · 1 skipped because their suggestion changed.", ChatActionPolicy.RenameResult(4, 0, 1));
        Assert.Equal("Described 10 files · 2 couldn't be described.", ChatActionPolicy.DescribeResult(new DeepAnalyzeComplete(10, 2, 3, "m", false)));
        Assert.Equal("Stopped after 4 files. Their descriptions are kept.", ChatActionPolicy.DescribeResult(new DeepAnalyzeComplete(4, 0, 3, "m", true)));
        Assert.Equal("Scan stopped. Files found so far stay in the library.", ChatActionPolicy.ScanResult(ScanPhase.Cancelled, 5, 0));
        Assert.Equal("Scan finished · 1,200 files · 3 couldn't be read.", ChatActionPolicy.ScanResult(ScanPhase.Completed, 1200, 3));
        Assert.Equal("Moved 7 files · 1 couldn't move.", ChatActionPolicy.MoveResult(new RestructureApplyResult(7, 1)));
        Assert.Equal("Moved 7 files back.", ChatActionPolicy.MovedBack(new RestructureApplyResult(7, 0)));
        Assert.Equal("Put back 2 copies.", ChatActionPolicy.RestoreResult(true, 2));
    }

    [Fact]
    public void TrashBatchComesFromTheEngineActionSuffix()
    {
        Assert.Equal("batch-7", ChatActionPolicy.TrashBatch(new BulkActionResult("trashFiles:batch-7", 1, 0, Array.Empty<BulkActionItem>())));
        Assert.Null(ChatActionPolicy.TrashBatch(new BulkActionResult("trashFiles", 1, 0, Array.Empty<BulkActionItem>())));
        Assert.Null(ChatActionPolicy.TrashBatch(new BulkActionResult("renameFiles:x", 1, 0, Array.Empty<BulkActionItem>())));
    }

    [Fact]
    public void ActionsRunOnlyFromTheirCardButtons()
    {
        var root = FindRepoRoot();
        var app = Path.Combine(root, "platforms", "windows", "src", "FileID.App");
        var callers = Directory.EnumerateFiles(app, "*.cs", SearchOption.AllDirectories)
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal))
            .Where(path => File.ReadAllText(path).Contains("_runner.RunAsync(", StringComparison.Ordinal) || File.ReadAllText(path).Contains("ChatActionRunner.Instance.RunAsync(", StringComparison.Ordinal))
            .Select(Path.GetFileName)
            .ToArray();
        Assert.Equal("ChatActionCard.cs", Assert.Single(callers));
        var card = File.ReadAllText(Path.Combine(app, "Views", "Workbenches", "ChatActionCard.cs"));
        var click = card.IndexOf("_primary.Click +=", StringComparison.Ordinal);
        Assert.True(click > 0);
        Assert.Contains("_runner.RunAsync(_action)", card[click..card.IndexOf('\n', click)], StringComparison.Ordinal);
        Assert.Equal(1, card.Split("_runner.RunAsync(").Length - 1);
        var chat = File.ReadAllText(Path.Combine(app, "Views", "Workbenches", "ChatWorkbench.cs"));
        Assert.Contains("ChatActionPolicy.Runnable(response.Actions)", chat, StringComparison.Ordinal);
    }

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "shared", "ipc-schema", "ipc.schema.json"))) return directory.FullName;
        }
        throw new DirectoryNotFoundException("The repository root was not found.");
    }
}
