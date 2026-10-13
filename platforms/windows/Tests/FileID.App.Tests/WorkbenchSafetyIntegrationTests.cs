using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class WorkbenchSafetyIntegrationTests
{
    [Theory]
    [InlineData("payload.exe")]
    [InlineData("payload.EXE")]
    [InlineData("payload.com")]
    [InlineData("payload.msi")]
    [InlineData("payload.scr")]
    [InlineData("run.bat")]
    [InlineData("run.cmd")]
    [InlineData("run.ps1")]
    [InlineData("run.vbs")]
    [InlineData("run.js")]
    [InlineData("run.hta")]
    [InlineData("shortcut.lnk")]
    [InlineData("shortcut.pif")]
    [InlineData("shortcut.url")]
    [InlineData("shortcut.website")]
    [InlineData("import.reg")]
    public void SharedSafeOpenPolicyRejectsExecutableScriptAndShortcutEvidence(string filename)
    {
        Assert.False(SafeOpen.IsAllowedFileExtension(filename));
    }

    [Theory]
    [InlineData("photo.JPG")]
    [InlineData("moment.mp4")]
    [InlineData("document.pdf")]
    [InlineData("markers.json")]
    public void SharedSafeOpenPolicyPreservesOrdinaryMediaAndDocumentEvidence(string filename)
    {
        Assert.True(SafeOpen.IsAllowedFileExtension(filename));
    }

    [Fact]
    public void EvidenceUsesEstablishedSafeOpenAndRevealWithoutBypassingNativeTimestampPreview()
    {
        var evidence = ReadSource("Views", "Workbenches", "EvidencePanel.cs");
        Assert.Contains("else if (!SafeOpen.TryOpenFile(hit.Path))", evidence, StringComparison.Ordinal);
        Assert.Contains("SafeOpen.Reveal(hit.Path)", evidence, StringComparison.Ordinal);
        Assert.DoesNotContain("Launcher.LaunchFileAsync", evidence, StringComparison.Ordinal);
        Assert.DoesNotContain("Process.Start", evidence, StringComparison.Ordinal);
        Assert.Contains("MediaSource.CreateFromStorageFile(file)", evidence, StringComparison.Ordinal);
        Assert.Contains("player.PlaybackSession.Position = TimeSpan.FromSeconds(seconds)", evidence, StringComparison.Ordinal);
        var policy = ReadSource("Services", "SafeOpen.cs");
        Assert.Contains("if (!IsAllowedFileExtension(path))", policy, StringComparison.Ordinal);
    }

    [Fact]
    public void AcceptedRecoveryGateEnablesUndoOnlyWithCompletedReceipts()
    {
        var plan = new ToolPreviewState();
        plan.Apply("history", new ToolResponse("r", "ok", "Last export", new[]
        {
            new ToolOutput(1, "source", "export", "completed", "Original preserved")
        }, Array.Empty<ToolCapability>(), "receipt"));
        Assert.Equal("receipt", plan.UndoID);
        Assert.True(ToolUndoIntegration.RecoverySafetyAccepted);
        Assert.True(ToolUndoIntegration.CanUndo(plan.UndoID));
        Assert.False(ToolUndoIntegration.CanUndo(null));
        Assert.Equal("receipt", plan.UndoID);
        Assert.Contains("internal recovery storage", ToolUndoIntegration.Detail, StringComparison.Ordinal);
    }

    [Fact]
    public void UndoButtonAndHandlerShareGateBeforeConfirmationOrSending()
    {
        var source = ReadSource("Views", "Workbenches", "ToolsWorkbench.cs");
        Assert.Contains("_undo.IsEnabled = ToolUndoIntegration.CanUndo(_plan.UndoID)", source, StringComparison.Ordinal);
        var handler = source[source.IndexOf("private async Task UndoAsync", StringComparison.Ordinal)..];
        var guard = handler.IndexOf("if (!ToolUndoIntegration.CanUndo(_plan.UndoID))", StringComparison.Ordinal);
        Assert.True(guard >= 0 && guard < handler.IndexOf("ConfirmAsync", StringComparison.Ordinal));
        Assert.True(guard < handler.IndexOf("await SendAsync", StringComparison.Ordinal));
    }

    private static string ReadSource(params string[] segments)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            var root = Path.Combine(directory.FullName, "platforms", "windows", "src", "FileID.App");
            if (Directory.Exists(root)) return File.ReadAllText(Path.Combine(new[] { root }.Concat(segments).ToArray()));
            directory = directory.Parent;
        }
        throw new DirectoryNotFoundException("Windows app sources were not found.");
    }
}
