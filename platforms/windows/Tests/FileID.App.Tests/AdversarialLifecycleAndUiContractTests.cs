using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class EngineLifecycleCoordinatorTests
{
    [Fact]
    public void LatestIntentCancelsAndSupersedesEarlierIntent()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var start = coordinator.Begin(shouldRun: true);
        using var stop = coordinator.Begin(shouldRun: false);

        Assert.True(start.Token.IsCancellationRequested);
        Assert.False(start.IsCurrent);
        Assert.True(stop.IsCurrent);
        Assert.False(coordinator.ShouldRun);
        Assert.Equal(stop.Revision, coordinator.CurrentRevision);
    }

    [Fact]
    public void CallerCancellationDoesNotChangeDesiredLifecycleState()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var caller = new CancellationTokenSource();
        using var stop = coordinator.Begin(shouldRun: false, caller.Token);

        caller.Cancel();

        Assert.True(stop.Token.IsCancellationRequested);
        Assert.True(stop.IsCurrent);
        Assert.False(coordinator.ShouldRun);
    }

    [Fact]
    public void TerminalStopCannotBeSupersededUntilCloseIsAborted()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var terminalStop = coordinator.BeginTerminalStop();

        Assert.True(coordinator.TerminalStopActive);
        Assert.Throws<InvalidOperationException>(
            () => coordinator.Begin(shouldRun: true));
        Assert.True(terminalStop.IsCurrent);
        Assert.True(terminalStop.PreviousShouldRun);
        Assert.False(coordinator.ShouldRun);

        Assert.False(
            coordinator.ReleaseTerminalStop(terminalStop.Revision + 1));
        Assert.True(coordinator.TerminalStopActive);
        Assert.True(coordinator.ReleaseTerminalStop(terminalStop.Revision));
        using var restart = coordinator.Begin(shouldRun: true);

        Assert.False(coordinator.TerminalStopActive);
        Assert.True(restart.IsCurrent);
        Assert.True(coordinator.ShouldRun);
    }

    [Fact]
    public async Task TerminalStopRejectsConcurrentRestartAttempts()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var terminalStop = coordinator.BeginTerminalStop();
        var attempts = new Task[64];
        for (var i = 0; i < attempts.Length; i++)
        {
            attempts[i] = Task.Run(() =>
                Assert.Throws<InvalidOperationException>(
                    () => coordinator.Begin(shouldRun: true)));
        }

        await Task.WhenAll(attempts).WaitAsync(TimeSpan.FromSeconds(2));

        Assert.True(terminalStop.IsCurrent);
        Assert.True(coordinator.TerminalStopActive);
        Assert.False(coordinator.ShouldRun);
    }

    [Fact]
    public void TerminalStopSurvivesRejectedRestartCancellationCallback()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var restart = coordinator.Begin(shouldRun: true);
        using var registration = restart.Token.Register(
            () => coordinator.Begin(shouldRun: true));

        using var terminalStop = coordinator.BeginTerminalStop();

        Assert.True(restart.Token.IsCancellationRequested);
        Assert.True(terminalStop.IsCurrent);
        Assert.True(coordinator.TerminalStopActive);
        Assert.False(coordinator.ShouldRun);
    }

    [Theory]
    [InlineData(true, false, false, true)]
    [InlineData(false, false, false, false)]
    [InlineData(true, true, false, false)]
    [InlineData(true, false, true, false)]
    public void FinalCloseRequiresTerminalStopAndZeroEngine(
        bool terminalStopActive,
        bool startInFlight,
        bool processAlive,
        bool expected)
        => Assert.Equal(
            expected,
            EngineClient.IsSafeToFinalizeApplicationClose(
                terminalStopActive,
                startInFlight,
                processAlive));

    [Fact]
    public async Task SupersessionCancellationCanReenterCoordinator()
    {
        var coordinator = new EngineLifecycleCoordinator();
        var first = coordinator.Begin(shouldRun: true);
        EngineLifecycleIntent? reentrant = null;
        var registration = first.Token.Register(
            () => reentrant = coordinator.Begin(shouldRun: false));

        var middle = await Task.Run(() => coordinator.Begin(shouldRun: true))
            .WaitAsync(TimeSpan.FromSeconds(2));

        var last = Assert.IsType<EngineLifecycleIntent>(reentrant);
        Assert.True(last.IsCurrent);
        Assert.False(middle.IsCurrent);
        Assert.False(coordinator.ShouldRun);

        registration.Unregister();
        first.Dispose();
        middle.Dispose();
        last.Dispose();
    }

    [Fact]
    public async Task LaterStopPreventsDelayedRestartPhase()
    {
        var coordinator = new EngineLifecycleCoordinator();
        using var restart = coordinator.Begin(shouldRun: true);
        var entered = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var spawned = false;

        var delayedRestart = Task.Run(async () =>
        {
            entered.TrySetResult();
            await release.Task.WaitAsync(restart.Token);
            restart.Token.ThrowIfCancellationRequested();
            if (restart.IsCurrent)
            {
                spawned = true;
            }
        });

        await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));
        using var stop = coordinator.Begin(shouldRun: false);
        release.TrySetResult();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => delayedRestart);
        Assert.False(spawned);
        Assert.True(stop.IsCurrent);
    }
}

public sealed class AdversarialUiSafetyContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Fact]
    public void TextEditingOwnsSelectAllAndUndoAccelerators()
    {
        var focusGuard = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Services",
            "KeyboardFocusGuard.cs"));
        Assert.Contains("current is TextBox or RichEditBox or PasswordBox",
            focusGuard, StringComparison.Ordinal);
        Assert.Contains("VisualTreeHelper.GetParent(current)",
            focusGuard, StringComparison.Ordinal);

        var library = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Library",
            "LibraryView.xaml.cs"));
        var selectAll = Slice(
            library,
            "private void OnSelectAllAccelerator",
            "private void OnUndoAccelerator");
        Assert.Contains("KeyboardFocusGuard.IsTextEditing(XamlRoot)",
            selectAll, StringComparison.Ordinal);
        Assert.True(
            selectAll.IndexOf("KeyboardFocusGuard.IsTextEditing", StringComparison.Ordinal)
            < selectAll.IndexOf("args.Handled = true", StringComparison.Ordinal));

        var undo = Slice(
            library,
            "private void OnUndoAccelerator",
            "private void OnKindChanged");
        Assert.Contains("KeyboardFocusGuard.IsTextEditing(XamlRoot)",
            undo, StringComparison.Ordinal);
        Assert.Contains("!UndoStack.Instance.CanUndo",
            undo, StringComparison.Ordinal);
        Assert.True(
            undo.IndexOf("!UndoStack.Instance.CanUndo", StringComparison.Ordinal)
            < undo.IndexOf("args.Handled = true", StringComparison.Ordinal));
    }

    [Fact]
    public void WindowAcceleratorsOnlyHandleActionsThatWereRouted()
    {
        var mainWindow = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App",
            "MainWindow.xaml.cs"));
        var shortcuts = Slice(
            mainWindow,
            "private void WireKeyboardShortcuts",
            "private void AddAccelerator");
        Assert.Contains("KeyboardFocusGuard.IsTextEditing",
            shortcuts, StringComparison.Ordinal);
        Assert.Contains("!Services.UndoStack.Instance.CanUndo",
            shortcuts, StringComparison.Ordinal);
        Assert.Contains("if (SearchFocusRequested is null) return;",
            shortcuts, StringComparison.Ordinal);

        var wrapper = Slice(
            mainWindow,
            "private void AddAccelerator",
            "public event EventHandler? SearchFocusRequested");
        Assert.DoesNotContain("Handled = true", wrapper, StringComparison.Ordinal);
    }

    [Fact]
    public void CloseConfirmationFailureKeepsWindowOpen()
    {
        var mainWindow = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App",
            "MainWindow.xaml.cs"));
        var failure = Slice(
            mainWindow,
            "DebugLog.Warn(\"Close-confirm dialog failed:",
            "if (!proceed)");

        Assert.Contains("proceed = false;", failure, StringComparison.Ordinal);
        Assert.DoesNotContain("proceed = true;", failure, StringComparison.Ordinal);
    }

    [Fact]
    public void CloseSequenceRechecksPendingAndProvesZeroEngineBeforeClose()
    {
        var mainWindow = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App",
            "MainWindow.xaml.cs"));
        var sequence = Slice(
            mainWindow,
            "private async Task RunCloseSequenceAsync",
            "/// <summary>Returns true when the close should proceed");

        var stop = sequence.IndexOf(
            "StopForApplicationCloseAsync",
            StringComparison.Ordinal);
        var dispatcherDrain = sequence.IndexOf(
            "await DrainCloseDispatcherAsync();",
            StringComparison.Ordinal);
        var latePending = sequence.IndexOf(
            "while (Services.ChangeLog.Instance.PendingCount > 0",
            StringComparison.Ordinal);
        var zeroEngineProof = sequence.IndexOf(
            "closeStop.TryCommit()",
            StringComparison.Ordinal);
        var finalClose = sequence.IndexOf(
            "_closeFinalized = true;",
            StringComparison.Ordinal);

        Assert.True(stop >= 0);
        Assert.True(dispatcherDrain > stop);
        Assert.True(latePending > dispatcherDrain);
        Assert.True(zeroEngineProof > latePending);
        Assert.True(finalClose > zeroEngineProof);
        Assert.Contains(
            "await closeStop.AbortAsync();",
            sequence,
            StringComparison.Ordinal);
        Assert.Contains(
            "_closeSequenceRunning = false;",
            sequence,
            StringComparison.Ordinal);
    }

    [Fact]
    public void ExcludedFolderPurgeFencesRemovalAndStaleCompletionUi()
    {
        var settings = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Settings",
            "SettingsView.xaml.cs"));

        Assert.Contains(
            "IsEnabled = !_excludedPurgeGenerations.ContainsKey(path)",
            settings, StringComparison.Ordinal);
        Assert.Contains(
            "if (_excludedPurgeGenerations.ContainsKey(path))",
            settings, StringComparison.Ordinal);
        Assert.Contains(
            "if (IsCurrentExcludedPurge(picked, purgeGeneration))",
            settings, StringComparison.Ordinal);
        Assert.Contains(
            "_excludedPurgeGenerations.Remove(picked)",
            settings, StringComparison.Ordinal);

        var guard = Slice(
            settings,
            "private bool IsCurrentExcludedPurge",
            "private void OnRemoveExcludedFolderClicked");
        Assert.Contains("!_unloaded", guard, StringComparison.Ordinal);
        Assert.Contains("_excludedPurgeGenerations.TryGetValue",
            guard, StringComparison.Ordinal);
        Assert.Contains("Settings.ExcludedFolders.Exists",
            guard, StringComparison.Ordinal);
    }

    [Fact]
    public void QueuedHardwareReprobeUiChecksUnloadStateAtExecutionTime()
    {
        var settings = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Settings",
            "SettingsView.xaml.cs"));
        var branch = Slice(
            settings,
            "nameof(EngineClient.LastHardwareReprobe)",
            "private void SyncNvidiaSection");

        Assert.Contains(
            "DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncReprobeUi(); });",
            branch, StringComparison.Ordinal);
    }

    [Fact]
    public void EngineLifecycleCallbacksCarryAndRecheckIntentRevision()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels",
            "EngineClient.cs"));
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels",
            "EngineClient.Commands.cs"));

        var start = Slice(
            client,
            "public async Task StartAsync()",
            "private void TerminateSupersededStart");
        Assert.Contains("_lifecycle.Begin(shouldRun: true)",
            start, StringComparison.Ordinal);
        Assert.Contains("StartCoreAsync(intent.Revision, intent.Token)",
            start, StringComparison.Ordinal);
        Assert.Contains(
            "ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken)",
            start, StringComparison.Ordinal);

        var exit = Slice(
            client,
            "private void OnProcessExited",
            "private async Task StartAfterCrashDelayAsync");
        Assert.Contains("ResolveCurrentExpectedExitRestartRevision()",
            exit, StringComparison.Ordinal);
        Assert.Contains("StartAfterCrashDelayAsync(delay, respawnRevision)",
            exit, StringComparison.Ordinal);
        Assert.DoesNotContain("StartAsync()", exit, StringComparison.Ordinal);

        var delayed = Slice(
            client,
            "private async Task StartAfterCrashDelayAsync",
            "private void ResetProcessBoundScanState");
        Assert.Contains("StartIfCurrentAsync",
            delayed, StringComparison.Ordinal);
        Assert.Contains(
            "_lifecycle.IsCurrent",
            delayed, StringComparison.Ordinal);

        var shutdown = Slice(
            commands,
            "public async Task ShutdownAsync()",
            "public async Task<bool> StopAndWaitForExitAsync");
        Assert.Contains("_lifecycle.Begin(shouldRun: false)",
            shutdown, StringComparison.Ordinal);
        Assert.Contains("new ShutdownCommand()",
            shutdown, StringComparison.Ordinal);
        Assert.Contains("intent.Token",
            shutdown, StringComparison.Ordinal);
        Assert.True(
            shutdown.Split(
                "ThrowIfLifecycleIntentSuperseded(intent)",
                StringSplitOptions.None).Length >= 4);

        var restart = Slice(
            commands,
            "public async Task RestartAsync",
            "public Task RunFaceClusteringAsync");
        Assert.Contains("_lifecycle.Begin(",
            restart, StringComparison.Ordinal);
        Assert.Contains("StopAndWaitForExitCoreAsync",
            restart, StringComparison.Ordinal);
        Assert.Contains("StartCoreAsync(intent.Revision, intent.Token)",
            restart, StringComparison.Ordinal);
        Assert.Contains("WaitForReadyAsync(",
            restart, StringComparison.Ordinal);
    }

    private static string Slice(string source, string start, string end)
    {
        var startIndex = source.IndexOf(start, StringComparison.Ordinal);
        Assert.True(startIndex >= 0, $"Missing source anchor: {start}");
        var endIndex = source.IndexOf(end, startIndex + start.Length,
            StringComparison.Ordinal);
        Assert.True(endIndex > startIndex, $"Missing source anchor: {end}");
        return source[startIndex..endIndex];
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
        throw new DirectoryNotFoundException("FileID repository root not found");
    }
}
