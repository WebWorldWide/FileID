using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Views.Library;
using Xunit;

namespace FileID.App.Tests;

public sealed class BulkActionJournalingTests
{
    [Fact]
    public void ConfirmedSuccessesRequireMatchingPerFileAndAggregateTruth()
    {
        var partial = Result(
            "renameFiles",
            1,
            1,
            new BulkActionItem(1, true),
            new BulkActionItem(2, false, "locked"));

        Assert.Equal(
            [1L],
            BulkActionResultTruth.ConfirmedSuccessfulFileIds(partial, [1L, 2L]));
        Assert.Empty(BulkActionResultTruth.ConfirmedSuccessfulFileIds(
            partial with { Succeeded = 2 },
            [1L, 2L]));
        Assert.Empty(BulkActionResultTruth.ConfirmedSuccessfulFileIds(
            Result("renameFiles", 1, 0, new BulkActionItem(3, true)),
            [1L, 2L]));
        Assert.Empty(BulkActionResultTruth.ConfirmedSuccessfulFileIds(
            Result("renameFiles", 1, 0, new BulkActionItem(null, true)),
            [1L, 2L]));
    }

    [Fact]
    public void ExactSuccessRequiresEveryExpectedIdAndNoFailures()
    {
        Assert.True(BulkActionResultTruth.ConfirmsExactSuccess(
            Result(
                "renameFiles",
                2,
                0,
                new BulkActionItem(1, true),
                new BulkActionItem(2, true)),
            [1L, 2L]));
        Assert.False(BulkActionResultTruth.ConfirmsExactSuccess(
            Result(
                "renameFiles",
                1,
                1,
                new BulkActionItem(1, true),
                new BulkActionItem(2, false)),
            [1L, 2L]));
    }

    [Fact]
    public void RenameInverseContainsOnlyTerminalConfirmedSuccesses()
    {
        BulkRenameSheet.RenamePlan[] plans =
        [
            new()
            {
                FileId = 1,
                CurrentPath = @"C:\Photos\before-one.jpg",
                ProposedName = "after-one.jpg",
            },
            new()
            {
                FileId = 2,
                CurrentPath = @"C:\Photos\before-two.jpg",
                ProposedName = "after-two.jpg",
            },
        ];
        var result = Result(
            "renameFiles",
            1,
            1,
            new BulkActionItem(1, true),
            new BulkActionItem(2, false, "occupied"));

        var inverse = BulkRenameSheet.BuildConfirmedInverse(plans, result);

        var entry = Assert.Single(inverse);
        Assert.Equal(1, entry.FileId);
        Assert.Equal("before-one.jpg", entry.NewName);
    }

    [Fact]
    public async Task RenameReverseAwaitsAndRequiresExactTerminalTruth()
    {
        RenameEntry[] inverse =
        [
            new(1, "before-one.jpg"),
            new(2, "before-two.jpg"),
        ];
        var terminal = new TaskCompletionSource<BulkActionResult>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var reverse = BulkRenameSheet.ReverseConfirmedAsync(inverse, _ => terminal.Task);

        Assert.False(reverse.IsCompleted);
        terminal.SetResult(Result(
            "renameFiles",
            1,
            1,
            new BulkActionItem(1, true),
            new BulkActionItem(2, false, "occupied")));
        Assert.False(await reverse);

        Assert.True(await BulkRenameSheet.ReverseConfirmedAsync(
            inverse,
            _ => Task.FromResult(Result(
                "renameFiles",
                2,
                0,
                new BulkActionItem(1, true),
                new BulkActionItem(2, true)))));
    }

    [Fact]
    public void PartialTagReplaceGroupsOnlyConfirmedSuccessfulIds()
    {
        var prior = new Dictionary<long, List<string>>
        {
            [1] = ["family"],
            [2] = ["work"],
        };
        var result = Result(
            "applyTags",
            1,
            1,
            new BulkActionItem(1, true),
            new BulkActionItem(2, false, "locked"));
        var confirmed =
            BulkActionResultTruth.ConfirmedSuccessfulFileIds(result, [1L, 2L]);

        var groups = BulkTagSheet.GroupByTagSet(confirmed, prior);

        var group = Assert.Single(groups);
        Assert.Equal([1L], group.Ids);
        Assert.Equal(["family"], group.Tags);
    }

    [Theory]
    [InlineData("add", 2, "add tags to 2 files")]
    [InlineData("remove", 1, "remove tags from 1 file")]
    [InlineData("replace", 3, "replace tags on 3 files")]
    public void TagHistoryLabelsDiscloseTheForwardMode(
        string mode,
        int fileCount,
        string expected)
    {
        Assert.Equal(expected, TagChangeJournal.FormatLabel(mode, fileCount));
    }

    [Fact]
    public async Task TagReverseValidatesEveryGroupAndStopsAtFirstUnconfirmedTerminal()
    {
        List<(List<long> Ids, List<string> Tags)> groups =
        [
            ([1], ["family"]),
            ([2], ["work"]),
            ([3], []),
        ];
        var calls = 0;

        var confirmed = await BulkTagSheet.RestoreGroupsConfirmedAsync(
            groups,
            (ids, _) =>
            {
                calls++;
                var fileId = ids[0];
                return Task.FromResult(calls == 2
                    ? Result("applyTags", 0, 1, new BulkActionItem(fileId, false, "locked"))
                    : Result("applyTags", 1, 0, new BulkActionItem(fileId, true)));
            });

        Assert.False(confirmed);
        Assert.Equal(2, calls);
    }

    [Fact]
    public async Task TagReverseSucceedsOnlyAfterEveryGroupIsConfirmed()
    {
        List<(List<long> Ids, List<string> Tags)> groups =
        [
            ([1], ["family"]),
            ([2], ["work"]),
            ([3], []),
        ];
        var calls = 0;

        var confirmed = await BulkTagSheet.RestoreGroupsConfirmedAsync(
            groups,
            (ids, _) =>
            {
                calls++;
                return Task.FromResult(
                    Result("applyTags", 1, 0, new BulkActionItem(ids[0], true)));
            });

        Assert.True(confirmed);
        Assert.Equal(3, calls);
    }

    [Fact]
    public void ScopedPersonTagReplacementPreservesUnrelatedTagsAndDeduplicatesNewName()
    {
        var prior = new Dictionary<long, List<string>>
        {
            [1] = ["Family", "alex"],
            [2] = ["Vacation", "Alex Morgan"],
            [3] = ["School"],
        };

        var groups = TagChangeJournal.BuildScopedReplacementGroups(
            [1, 2, 3],
            prior,
            "Alex",
            "Alex Morgan");
        var byId = groups
            .SelectMany(group => group.Ids.Select(id => (Id: id, group.Tags)))
            .ToDictionary(entry => entry.Id, entry => entry.Tags);

        Assert.Equal(["Alex Morgan", "Family"], byId[1]);
        Assert.Equal(["Alex Morgan", "Vacation"], byId[2]);
        Assert.Equal(["Alex Morgan", "School"], byId[3]);
    }

    [Fact]
    public void ProductionFlowsJournalOnlyAfterTerminalAndUseConfirmedIds()
    {
        var root = FindRepoRoot();
        var rename = File.ReadAllText(Path.Combine(
            root,
            "platforms",
            "windows",
            "src",
            "FileID.App",
            "Views",
            "Library",
            "BulkRenameSheet.xaml.cs"));
        var tag = File.ReadAllText(Path.Combine(
            root,
            "platforms",
            "windows",
            "src",
            "FileID.App",
            "Views",
            "Library",
            "BulkTagSheet.xaml.cs"));

        var renameWait = rename.IndexOf(
            "var result = await EngineClient.Instance.WaitForBulkActionResultAsync",
            StringComparison.Ordinal);
        var inverse = rename.IndexOf(
            "var inverse = BuildConfirmedInverse",
            StringComparison.Ordinal);
        var renamePush = rename.IndexOf(
            "Services.UndoStack.Instance.Push",
            StringComparison.Ordinal);
        Assert.True(renameWait >= 0 && inverse > renameWait && renamePush > inverse);

        var tagWait = tag.IndexOf(
            "var result = await EngineClient.Instance.WaitForBulkActionResultAsync",
            StringComparison.Ordinal);
        var confirmedIds = tag.IndexOf(
            "var confirmedFileIds = Services.BulkActionResultTruth",
            StringComparison.Ordinal);
        var journal = tag.IndexOf(
            "Services.TagChangeJournal.PushUndo(",
            StringComparison.Ordinal);
        Assert.True(tagWait >= 0 && confirmedIds > tagWait && journal > confirmedIds);
        Assert.Contains(
            "CapturePriorUserTagsAsync(_fileIds)",
            tag,
            StringComparison.Ordinal);
        Assert.DoesNotContain(
            "mode == \"replace\" && confirmedFileIds",
            tag,
            StringComparison.Ordinal);
    }

    private static BulkActionResult Result(
        string action,
        uint succeeded,
        uint failed,
        params BulkActionItem[] messages)
        => new(action, succeeded, failed, messages);

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory);
             directory is not null;
             directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(directory.FullName, "platforms", "windows")))
            {
                return directory.FullName;
            }
        }
        throw new DirectoryNotFoundException(
            "Could not find the FileID repository root from the test output directory.");
    }
}
