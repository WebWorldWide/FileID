using Xunit;

namespace FileID.App.Tests;

public sealed class PreviewLifecycleContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void PreviewStartsAfterDialogOpenedAndClosesAfterShowReturns()
    {
        var library = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Library", "LibraryView.xaml.cs"));
        var opened = library.IndexOf("dialog.Opened +=", StringComparison.Ordinal);
        var show = library.IndexOf("await dialog.ShowAsync()", opened, StringComparison.Ordinal);
        var close = library.IndexOf("finally { sheet.CloseFromHost(); }", show, StringComparison.Ordinal);

        Assert.True(opened >= 0, "Preview load must be wired to ContentDialog.Opened.");
        Assert.True(show > opened, "Preview must not start before the dialog is opened.");
        Assert.True(close > show, "Preview teardown must run after ShowAsync returns.");
    }

    [Fact]
    public void PreviewDoesNotTreatTransientUnloadedAsTerminalClose()
    {
        var preview = PreviewSource();

        Assert.DoesNotContain("Unloaded +=", preview, StringComparison.Ordinal);
        Assert.Contains("Only ShowAsync completion is a terminal close signal", preview, StringComparison.Ordinal);
        Assert.Contains("internal void CloseFromHost()", preview, StringComparison.Ordinal);
    }

    [Fact]
    public void ThumbnailReadRequiresTheCompleteAdvertisedPayload()
    {
        var preview = PreviewSource();

        Assert.Contains("var loaded = await reader.LoadAsync(size);", preview, StringComparison.Ordinal);
        Assert.Contains("if (loaded != size)", preview, StringComparison.Ordinal);
        Assert.DoesNotContain("InputStreamOptions.None", preview, StringComparison.Ordinal);
    }

    [Fact]
    public void DirectImageFallbackIsBoundedAndStreamed()
    {
        var preview = PreviewSource();

        Assert.Contains("MaxDirectPreviewEncodedBytes", preview, StringComparison.Ordinal);
        Assert.Contains("stream = await file.OpenReadAsync();", preview, StringComparison.Ordinal);
        Assert.Contains("stream.Size > (ulong)MaxDirectPreviewEncodedBytes", preview, StringComparison.Ordinal);
        Assert.DoesNotContain("File.ReadAllBytesAsync(path)", preview, StringComparison.Ordinal);
    }

    private static string PreviewSource()
        => File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Library", "FilePreviewSheet.xaml.cs"));

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
