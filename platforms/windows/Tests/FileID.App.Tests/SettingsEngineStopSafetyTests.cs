using FileID.ViewModels;
using FileID.Views.Sidebar;
using Xunit;

namespace FileID.App.Tests;

public sealed class SettingsEngineStopSafetyTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Theory]
    [InlineData(null, "FileID's helper stopped")]
    [InlineData("", "FileID's helper stopped")]
    [InlineData("   ", "FileID's helper stopped")]
    [InlineData("Engine stopped", "FileID's helper is off")]
    [InlineData("Engine crashed three times", "FileID's helper stopped")]
    public void CrashedStatusTextIsNeverBlank(string? reason, string expected)
        => Assert.Equal(expected, SidebarEngineStatus.ResolveCrashedStatusText(reason));

    [Theory]
    [InlineData(null, "FileID's helper stopped unexpectedly. Quit and reopen FileID.")]
    [InlineData("", "FileID's helper stopped unexpectedly. Quit and reopen FileID.")]
    [InlineData("Engine stopped", "FileID's helper is off. Turn it back on in Settings.")]
    [InlineData("Engine failed", "FileID's helper stopped. Quit and reopen FileID. Details: Engine failed")]
    public void CrashedStatusTipExplainsStoppedAndCrashStates(string? reason, string expected)
        => Assert.Equal(expected, SidebarEngineStatus.ResolveCrashedStatusTip(reason));

    [Fact]
    public void ExpectedExitClearsScanPresentationBeforePublishingStoppedState()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var callbackStart = client.IndexOf("private void OnProcessExited", StringComparison.Ordinal);
        var start = client.IndexOf("if (expectedExit)", callbackStart, StringComparison.Ordinal);
        var end = client.IndexOf("var delay = _consecutiveFailures switch", start, StringComparison.Ordinal);

        Assert.True(callbackStart >= 0 && start > callbackStart && end > start);
        var reset = client.IndexOf("ResetProcessBoundScanState();", callbackStart, StringComparison.Ordinal);
        Assert.True(reset > callbackStart && reset < start,
            "Process-bound scan presentation must be retired before expected-exit state is published.");
        var expectedExit = client[start..end];
        var reason = expectedExit.IndexOf("CrashReason = StoppedReason;", StringComparison.Ordinal);
        var state = expectedExit.IndexOf("State = LifecycleState.Crashed;", StringComparison.Ordinal);

        Assert.True(
            reason >= 0 && state > reason,
            "Expected exits must retire active scan UI before publishing a coherent stopped state.");
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
