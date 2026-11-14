using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class PersonMergePolicyTests
{
    private static BulkActionResult Result(uint succeeded, uint failed, params BulkActionItem[] items) =>
        new("mergeClusters", succeeded, failed, items);

    [Fact]
    public void TheMergeRecordIdComesFromTheConfirmedItem()
    {
        var result = Result(1, 0, new BulkActionItem(null, true, "Merged.", null, "op-42"));

        Assert.True(PersonMergePolicy.Succeeded(result));
        Assert.Equal("op-42", PersonMergePolicy.OperationId(result));
    }

    [Fact]
    public void AnOlderHelperThatSendsNoRecordIdFallsBackToTheFaceList()
    {
        Assert.Null(PersonMergePolicy.OperationId(Result(1, 0, new BulkActionItem(null, true, "Merged."))));
        Assert.Null(PersonMergePolicy.OperationId(Result(1, 0, new BulkActionItem(null, true, null, null, "  "))));
        Assert.Null(PersonMergePolicy.OperationId(Result(1, 0)));
        Assert.Null(PersonMergePolicy.OperationId(Result(0, 1, new BulkActionItem(null, false, "no", null, "op-9"))));
    }

    [Fact]
    public void AFailedMergeReadsTheHelpersSentenceOrAPlainOne()
    {
        var refused = Result(0, 1, new BulkActionItem(null, false, " Those two are already one person. "));

        Assert.False(PersonMergePolicy.Succeeded(refused));
        Assert.Equal("Those two are already one person.", PersonMergePolicy.FailureText(refused));
        Assert.Equal(PersonMergePolicy.CouldNotMerge, PersonMergePolicy.FailureText(Result(0, 0)));
        Assert.False(PersonMergePolicy.Succeeded(Result(0, 0)));
    }

    [Fact]
    public void EveryMergeConfirmationPromisesUndo()
    {
        Assert.Equal("Merge these people? You can undo this right after.", PersonMergePolicy.ConfirmText);
        Assert.EndsWith(PersonMergePolicy.UndoHint, PersonMergePolicy.ConfirmText, StringComparison.Ordinal);
    }
}
