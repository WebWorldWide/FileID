using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Views.Restructure;
using Xunit;

namespace FileID.App.Tests;

public class RestructureUndoRoutingTests
{
    [Theory]
    [InlineData(true, false, false, true)]
    [InlineData(true, true, false, false)]
    [InlineData(true, false, true, false)]
    [InlineData(false, false, false, false)]
    public void RestructureUndo_StartsOnlyWhenItsRootIsUndoableAndNoUndoIsActive(
        bool canUndoForRoot,
        bool changeLogUndoInFlight,
        bool engineUndoInFlight,
        bool expected)
        => Assert.Equal(
            expected,
            RestructureView.CanStartRestructureUndo(
                canUndoForRoot,
                changeLogUndoInFlight,
                engineUndoInFlight));

    [Fact]
    public void CancelledUndo_ReportsRestoredAndRemainingCounts()
    {
        var result = new RestructureApplyResult(
            Applied: 4,
            Failed: 0,
            Cancelled: true,
            Planned: 10,
            Remaining: 6);

        var status = RestructureView.FormatUndoCompletion(result);

        Assert.Contains("restoring 4 files", status);
        Assert.Contains("6 moves still need to be undone", status);
        Assert.Contains("Click Undo again", status);
    }

    [Fact]
    public void EmptyUndoResult_DoesNotClaimSuccess()
    {
        var status = RestructureView.FormatUndoCompletion(
            new RestructureApplyResult(Applied: 0, Failed: 0));

        Assert.Contains("Nothing was restored", status);
        Assert.DoesNotContain("Undid the last restructure", status);
    }

    [Fact]
    public void ShortcutUndo_ReportsRemovedLinksWithoutClaimingFilesMoved()
    {
        var status = RestructureView.FormatUndoCompletion(
            new RestructureApplyResult(Applied: 4, Failed: 0),
            wasShortcutUndo: true);

        Assert.Contains("Removed 4 restructure shortcuts", status);
        Assert.DoesNotContain("moved back", status);
        Assert.DoesNotContain("restored", status);
    }

    [Fact]
    public void CancelledRealMoveResult_ReportsExactStoppedAndRemainingCounts()
    {
        var result = new RestructureApplyResult(
            Applied: 7,
            Failed: 0,
            Cancelled: true,
            Planned: 20,
            Remaining: 13);

        var status = RestructureView.FormatApplyCompletion(
            result,
            appliedAsShortcuts: false);

        Assert.Contains("Stopped safely after moving 7 files", status);
        Assert.Contains("13 eligible proposals stayed unchanged", status);
        Assert.Contains("Completed moves remain undoable", status);
    }

    [Fact]
    public void CancelledShortcutResult_DoesNotClaimFilesMoved()
    {
        var result = new RestructureApplyResult(
            Applied: 3,
            Failed: 0,
            Cancelled: true,
            Planned: 5,
            Remaining: 2);

        var status = RestructureView.FormatApplyCompletion(
            result,
            appliedAsShortcuts: true);

        Assert.Contains("creating 3 shortcuts", status);
        Assert.Contains("Originals stayed put", status);
        Assert.DoesNotContain("moving", status);
    }

    [Theory]
    [InlineData(false, 1, true, true)]
    [InlineData(true, 1, true, false)]
    [InlineData(false, 0, true, false)]
    [InlineData(false, 1, false, false)]
    public void UndoableHistoryEntry_IsCreatedOnlyForRealMoves(
        bool appliedAsShortcuts,
        uint applied,
        bool canUndoThisRun,
        bool expected)
        => Assert.Equal(
            expected,
            RestructureView.ShouldRecordUndoableRestructureChange(
                appliedAsShortcuts,
                applied,
                canUndoThisRun));

    [Fact]
    public void LatestRetryableRestructureEntry_IsSelected()
    {
        var latest = Entry(ChangeKind.Restructure, ChangeStatus.UndoFailed);
        var older = Entry(ChangeKind.Restructure, ChangeStatus.Undoable);

        Assert.Same(
            latest,
            RestructureView.FindLatestRestructureUndoEntry([latest, older]));
    }

    [Fact]
    public void NonRetryableAndUnrelatedEntries_AreSkipped()
    {
        var unrelated = Entry(ChangeKind.Trash, ChangeStatus.Undoable);
        var superseded = Entry(ChangeKind.Restructure, ChangeStatus.NotUndoable);
        var retryable = Entry(ChangeKind.Restructure, ChangeStatus.UndoFailed);

        Assert.Same(
            retryable,
            RestructureView.FindLatestRestructureUndoEntry(
                [unrelated, superseded, retryable]));
    }

    private static ChangeLogEntry Entry(ChangeKind kind, ChangeStatus status)
    {
        var entry = new ChangeLogEntry(
            "test",
            kind,
            () => Task.FromResult(true));
        entry.Status = status;
        return entry;
    }
}
