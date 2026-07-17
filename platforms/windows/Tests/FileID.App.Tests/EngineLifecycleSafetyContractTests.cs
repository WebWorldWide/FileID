using Xunit;

namespace FileID.App.Tests;

public sealed class EngineLifecycleSafetyContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void StopTimeoutIsExplicitAndRestartFailsClosed()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));

        Assert.Contains("public async Task<bool> StopAndWaitForExitAsync", commands, StringComparison.Ordinal);
        Assert.Contains("return false;", commands, StringComparison.Ordinal);
        Assert.Contains("if (!await StopAndWaitForExitAsync", commands, StringComparison.Ordinal);
        Assert.Contains("restart was aborted", commands, StringComparison.Ordinal);
        Assert.Contains("restartAfterLateExit ? 1 : 0", commands, StringComparison.Ordinal);
        Assert.Contains("Volatile.Read(ref _isStarting) == 0", commands, StringComparison.Ordinal);

        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        Assert.Contains("Interlocked.Exchange(ref _restartAfterExpectedExit, 0) == 1", client, StringComparison.Ordinal);
        Assert.Contains("StartAfterLateExpectedExitAsync", client, StringComparison.Ordinal);
    }

    [Fact]
    public void FallbackWipeRefusesToDeleteAfterStopTimeout()
    {
        var wipe = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarFolderHeader.xaml.cs"));
        var stopCheck = wipe.IndexOf("if (!await EngineClient.Instance.StopAndWaitForExitAsync", StringComparison.Ordinal);
        var refusal = wipe.IndexOf("refusing to delete a live engine's database", stopCheck, StringComparison.Ordinal);
        var earlyReturn = wipe.IndexOf("return;", refusal, StringComparison.Ordinal);
        var deleteStage = wipe.IndexOf("[WIPE] stage 3: delete DB files", stopCheck, StringComparison.Ordinal);

        Assert.True(stopCheck >= 0, "Fallback wipe must inspect the stop result.");
        Assert.True(refusal > stopCheck, "Fallback wipe must report a live-engine refusal.");
        Assert.True(earlyReturn > refusal && earlyReturn < deleteStage,
            "Fallback wipe must return before any database deletion after a stop timeout.");
    }

    [Fact]
    public void EngineStartMarshalsToTheUiDispatcherBeforeObservableWrites()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var start = client.IndexOf("public async Task StartAsync()", StringComparison.Ordinal);
        var marshal = client.IndexOf("if (!_ui.HasThreadAccess)", start, StringComparison.Ordinal);
        var startingState = client.IndexOf("State = LifecycleState.Starting;", start, StringComparison.Ordinal);

        Assert.True(marshal > start, "StartAsync must check UI thread access.");
        Assert.True(startingState > marshal, "StartAsync must marshal before publishing lifecycle state.");
        Assert.Contains("_ui.TryEnqueue(async () =>", client[marshal..startingState], StringComparison.Ordinal);
    }

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
