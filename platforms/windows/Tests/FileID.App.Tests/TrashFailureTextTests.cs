using FileID.IpcSchema;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class TrashFailureTextTests
{
    private static BulkActionResult Result(uint succeeded, uint failed, params BulkActionItem[] items) =>
        new("trashFiles:batch", succeeded, failed, items);

    private static BulkActionItem Refused(long id, string? reason, string? message = null) =>
        new(id, false, message, reason);

    [Theory]
    [InlineData("changed", 1, "1 file changed since the scan, so FileID left it alone. Scan again, then retry.")]
    [InlineData("changed", 3, "3 files changed since the scan, so FileID left them alone. Scan again, then retry.")]
    [InlineData("protected", 1, "1 file is in a protected location that FileID doesn't change.")]
    [InlineData("protected", 2, "2 files are in a protected location that FileID doesn't change.")]
    [InlineData("missing", 1, "1 file is no longer there.")]
    [InlineData("missing", 4, "4 files are no longer there.")]
    [InlineData("failed", 1, "1 file couldn't be moved. It may be open in another app, or you may not have permission.")]
    [InlineData("failed", 2, "2 files couldn't be moved. They may be open in another app, or you may not have permission.")]
    public void EachReasonReadsAsAPlainSentence(string reason, int count, string expected)
    {
        Assert.Equal(expected, TrashFailureText.Sentence(reason, count));
    }

    [Fact]
    public void TheReasonWinsOverTheHelpersWording()
    {
        var item = Refused(1, "missing", "trash failed: os error 5");

        Assert.Equal(TrashFailureText.Missing, TrashFailureText.Kind(item));
    }

    [Theory]
    [InlineData("This location is read-only, so FileID won't move files out of it.", "protected")]
    [InlineData("This file belongs to a managed media library.", "protected")]
    [InlineData("File changed since it was indexed.", "changed")]
    [InlineData("This copy changed since it was checked. Nothing was moved.", "changed")]
    [InlineData("not found: no rows", "missing")]
    [InlineData("something else", "")]
    [InlineData(null, "")]
    public void AnOlderHelperIsReadFromItsFixedWording(string? message, string expected)
    {
        Assert.Equal(expected, TrashFailureText.Kind(Refused(1, null, message)));
    }

    [Fact]
    public void SummaryGroupsFailuresAndSkipsFilesThatMoved()
    {
        var result = Result(1, 4,
            new BulkActionItem(1, true),
            Refused(2, "changed"),
            Refused(3, "changed"),
            Refused(4, "protected"),
            Refused(5, "failed", "trash failed: os error 32"));

        Assert.Equal(
            "2 files changed since the scan, so FileID left them alone. Scan again, then retry. "
            + "1 file is in a protected location that FileID doesn't change. "
            + "1 file couldn't be moved. It may be open in another app, or you may not have permission.",
            TrashFailureText.Summary(result));
    }

    [Fact]
    public void SummaryFallsBackToTheMessageWhenThereIsNoReason()
    {
        Assert.Equal("The drive was unplugged.", TrashFailureText.Summary(Result(0, 1, Refused(1, null, "The drive was unplugged"))));
        Assert.Equal(
            "1 file couldn't be moved. It may be open in another app, or you may not have permission.",
            TrashFailureText.Summary(Result(0, 1, Refused(1, null))));
        Assert.Equal(
            "2 files couldn't be moved. They may be open in another app, or you may not have permission.",
            TrashFailureText.Summary(Result(0, 2)));
        Assert.Equal(string.Empty, TrashFailureText.Summary(Result(2, 0, new BulkActionItem(1, true), new BulkActionItem(2, true))));
    }
}
