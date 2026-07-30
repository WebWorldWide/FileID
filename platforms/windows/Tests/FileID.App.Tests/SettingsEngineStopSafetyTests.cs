using FileID.ViewModels;
using FileID.Views.Sidebar;
using Xunit;

namespace FileID.App.Tests;

public sealed class SettingsEngineStopSafetyTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Theory]
    [InlineData(null, "Engine crashed")]
    [InlineData("", "Engine crashed")]
    [InlineData("   ", "Engine crashed")]
    [InlineData("Engine stopped", "Engine stopped")]
    [InlineData("Engine crashed three times", "Engine crashed three times")]
    public void CrashedStatusTextIsNeverBlank(string? reason, string expected)
        => Assert.Equal(expected, SidebarEngineStatus.ResolveCrashedStatusText(reason));

    [Theory]
    [InlineData(null, "Engine crashed. Check %LOCALAPPDATA%\\FileID\\logs\\app.log.")]
    [InlineData("", "Engine crashed. Check %LOCALAPPDATA%\\FileID\\logs\\app.log.")]
    [InlineData("Engine stopped", "Engine stopped. Restart it from Settings.")]
    [InlineData("Engine failed", "Engine failed")]
    public void CrashedStatusTipExplainsStoppedAndCrashStates(string? reason, string expected)
        => Assert.Equal(expected, SidebarEngineStatus.ResolveCrashedStatusTip(reason));

    [Fact]
    public void ExpectedExitClearsScanPresentationBeforePublishingStoppedState()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var start = client.IndexOf("if (expectedExit)", StringComparison.Ordinal);
        var end = client.IndexOf("var respawnRevision", start, StringComparison.Ordinal);

        Assert.True(start >= 0 && end > start);
        var expectedExit = client[start..end];
        var reset = expectedExit.IndexOf("ResetProcessBoundScanState();", StringComparison.Ordinal);
        var reason = expectedExit.IndexOf("CrashReason = StoppedReason;", StringComparison.Ordinal);
        var state = expectedExit.IndexOf("State = LifecycleState.Crashed;", StringComparison.Ordinal);

        Assert.True(
            reset >= 0 && reason > reset && state > reason,
            "Expected exits must retire active scan UI before publishing a coherent stopped state.");

        var stoppedIntentStart = client.IndexOf("var respawnRevision", end, StringComparison.Ordinal);
        var stoppedIntentEnd = client.IndexOf("// Auto-respawn with bounded backoff.", stoppedIntentStart, StringComparison.Ordinal);
        Assert.True(stoppedIntentStart >= 0 && stoppedIntentEnd > stoppedIntentStart);
        var stoppedIntent = client[stoppedIntentStart..stoppedIntentEnd];
        Assert.Contains("ResetProcessBoundScanState();", stoppedIntent, StringComparison.Ordinal);
        Assert.Contains("CrashReason = StoppedReason;", stoppedIntent, StringComparison.Ordinal);
        Assert.DoesNotContain("CrashReason = string.Empty;", client, StringComparison.Ordinal);
        Assert.Equal("Engine stopped", EngineClient.StoppedReason);
    }

    private static string PathInRepo(params string[] parts)
        => Path.Combine([RepoRoot, .. parts]);

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
        throw new DirectoryNotFoundException("FileID repository root not found.");
    }
}
