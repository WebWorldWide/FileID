using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class FolderPickerServiceTests
{
    [Fact]
    public void CancellationHResult_IsRecognizedWithoutTreatingOtherFailuresAsCancel()
    {
        Assert.True(FolderPickerService.IsCancellationHResult(unchecked((int)0x800704C7)));
        Assert.False(FolderPickerService.IsCancellationHResult(unchecked((int)0x80004005)));
        Assert.False(FolderPickerService.IsCancellationHResult(0));
    }

    [Fact]
    public void ValidateSelectedPath_AcceptsAnEmptyReadableFolder()
    {
        var path = Path.Combine(Path.GetTempPath(), "FileID-picker-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        try
        {
            var result = FolderPickerService.ValidateSelectedPath(path);

            Assert.Equal(path, result.Path);
            Assert.Null(result.FailureReason);
        }
        finally
        {
            Directory.Delete(path);
        }
    }

    [Fact]
    public void ValidateSelectedPath_RejectsMissingFolderWithActionableReason()
    {
        var path = Path.Combine(Path.GetTempPath(), "FileID-picker-missing-" + Guid.NewGuid().ToString("N"));

        var result = FolderPickerService.ValidateSelectedPath(path);

        Assert.Null(result.Path);
        Assert.Equal("That folder no longer exists.", result.FailureReason);
    }

    [Fact]
    public async Task PickFolderAsync_RejectsMissingOwnerWindowWithoutOpeningDialog()
    {
        var result = await FolderPickerService.PickFolderAsync(IntPtr.Zero);

        Assert.Null(result.Path);
        Assert.Contains("attach the folder picker", result.FailureReason, StringComparison.OrdinalIgnoreCase);
    }
}
