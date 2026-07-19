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

    [Fact]
    public void StaleEngineEventsCannotMutateTheReplacementProcessState()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));

        Assert.Contains("StdoutLoopAsync(p.StandardOutput, generation, ct)", client, StringComparison.Ordinal);
        var start = client.IndexOf("public async Task StartAsync()", StringComparison.Ordinal);
        var exitedCleanup = client.IndexOf("if (_process is { HasExited: true })", start, StringComparison.Ordinal);
        var resetScanState = client.IndexOf("ResetProcessBoundScanState();", exitedCleanup, StringComparison.Ordinal);
        var startingState = client.IndexOf("State = LifecycleState.Starting;", start, StringComparison.Ordinal);
        Assert.True(exitedCleanup > start && resetScanState > exitedCleanup && startingState > resetScanState,
            "A replacement spawn must retire its predecessor generation and process-bound scan state.");
        Assert.Contains("Phase = ScanPhase.Idle;", client, StringComparison.Ordinal);
        Assert.Contains("LastProgress = null;", client, StringComparison.Ordinal);
        Assert.Contains("LastBatch = null;", client, StringComparison.Ordinal);
        Assert.Contains("LastError = null;", client, StringComparison.Ordinal);
        Assert.Contains("_scanStartedAt = null;", client, StringComparison.Ordinal);

        var apply = client.IndexOf("private void Apply(IpcEvent ev, int generation)", StringComparison.Ordinal);
        var staleCheck = client.IndexOf("if (generation != SpawnGeneration)", apply, StringComparison.Ordinal);
        var publish = client.IndexOf("_events.OnNext(ev)", apply, StringComparison.Ordinal);

        Assert.True(apply >= 0 && staleCheck > apply, "Apply must receive the source process generation.");
        Assert.True(publish > staleCheck, "Stale events must be rejected before publication or state mutation.");
    }

    [Fact]
    public void ExpectedExitIsConsumedBeforeAStaleSenderReturns()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var callback = client.IndexOf("private void OnProcessExited", StringComparison.Ordinal);
        var consume = client.IndexOf("ConsumeExpectedExit(exited)", callback, StringComparison.Ordinal);
        var staleSender = client.IndexOf("if (!ReferenceEquals(sender, _process))", callback, StringComparison.Ordinal);

        Assert.True(consume > callback && staleSender > consume,
            "A late expected-exit callback must clear its own flag before returning as stale.");
        Assert.Contains("_expectedExitProcess", client, StringComparison.Ordinal);
    }

    [Fact]
    public void DeepAnalyzeReservationAndWaiterAreGenerationOwned()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        Assert.Contains("GenerationOwnedOperationSlot<DeepAnalyzeOperation>", commands, StringComparison.Ordinal);
        Assert.Contains("RetireDeepAnalyzeGeneration(int generation)", commands, StringComparison.Ordinal);
        Assert.Contains("private bool CompleteDeepAnalyzeCommand(", commands, StringComparison.Ordinal);
        Assert.Contains("owner.Payload.Completion?.TrySetException", commands, StringComparison.Ordinal);
        Assert.Contains("nameof(DeepAnalyzeCommandInFlight)", commands, StringComparison.Ordinal);
        Assert.DoesNotContain("private int _deepAnalyzeCommandInFlight;", commands, StringComparison.Ordinal);
        foreach (var method in new[] { "DeepAnalyzeFileAsync", "DeepAnalyzeFolderAsync", "DeepAnalyzeAllAsync" })
        {
            var start = commands.IndexOf(method, StringComparison.Ordinal);
            var reserve = commands.IndexOf("TryReserveDeepAnalyzeCommand(", start, StringComparison.Ordinal);
            var send = commands.IndexOf("SendCommandAsync", start, StringComparison.Ordinal);
            Assert.True(start >= 0 && reserve > start && reserve < send,
                $"{method} must reserve the generation-owned Deep Analyze slot before sending.");
        }

        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var cleanup = client.IndexOf("private void Cleanup()", StringComparison.Ordinal);
        var retire = client.IndexOf("RetireDeepAnalyzeGeneration(retiringGeneration);", cleanup, StringComparison.Ordinal);
        var generationBump = client.IndexOf("Interlocked.Increment(ref _spawnGeneration);", cleanup, StringComparison.Ordinal);
        Assert.True(retire > cleanup && generationBump > retire,
            "Cleanup must terminate the old generation's waiter before publishing a replacement generation.");
        var complete = client.IndexOf("case DeepAnalyzeCompleteEvent", StringComparison.Ordinal);
        var terminalClaim = client.IndexOf("CompleteDeepAnalyzeCommand(generation, dac.Result", complete, StringComparison.Ordinal);
        var presentation = client.IndexOf("DeepAnalyzeComplete = dac.Result;", complete, StringComparison.Ordinal);
        Assert.True(terminalClaim > complete && presentation > terminalClaim,
            "A fenced terminal must be claimed before it can mutate observable presentation.");
        Assert.Contains("_deepAnalyzePresentationGeneration) != retiringGeneration", client, StringComparison.Ordinal);
        Assert.Contains("FenceRejectedDeepAnalyzeCommand(generation", client, StringComparison.Ordinal);
        Assert.Contains("onWriteStarted?.Invoke();", commands, StringComparison.Ordinal);
        Assert.Contains("Volatile.Read(ref owner.Payload.SendBegan) == 0", commands, StringComparison.Ordinal);
        Assert.Contains("RestartAfterDeepAnalyzeFenceAsync(owner", commands, StringComparison.Ordinal);
        Assert.Contains("Interlocked.CompareExchange(ref owner.Payload.TerminalState, 1, 0)", commands, StringComparison.Ordinal);
        Assert.Contains("Interlocked.CompareExchange(ref owner.Payload.TerminalState, 2, 0)", commands, StringComparison.Ordinal);
        var publish = commands.IndexOf("publishPresentation();", StringComparison.Ordinal);
        var releaseAfterPublish = commands.IndexOf("_deepAnalyzeCommandSlot.Release(owner);", publish, StringComparison.Ordinal);
        Assert.True(publish >= 0 && releaseAfterPublish > publish,
            "The owner must stay installed until its terminal presentation is published.");

        var view = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "DeepAnalyze", "DeepAnalyzeView.xaml.cs"));
        Assert.Contains("case nameof(EngineClient.DeepAnalyzeCommandInFlight):", view, StringComparison.Ordinal);
        Assert.Contains("SyncDeepAnalyzeControls();", view, StringComparison.Ordinal);
        Assert.Contains("ec.DeepAnalyzeCommandAttemptId != attemptId", view, StringComparison.Ordinal);
        Assert.DoesNotContain("ResetOptimisticRunUi", view, StringComparison.Ordinal);

        var restructure = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Restructure", "RestructureView.xaml.cs"));
        Assert.Contains("case nameof(EngineClient.DeepAnalyzeCommandInFlight):", restructure, StringComparison.Ordinal);
        Assert.Contains("if (engine.DeepAnalyzeCommandInFlight)", restructure, StringComparison.Ordinal);
        Assert.Contains("if (EngineClient.Instance.DeepAnalyzeCommandInFlight) return;", restructure, StringComparison.Ordinal);
    }

    [Fact]
    public void ScanStartOwnsOptimismAndRollsBackConditionally()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        var start = commands.IndexOf("public async Task StartScanAsync", StringComparison.Ordinal);
        var reserve = commands.IndexOf("_scanStartSlot.TryReserve", start, StringComparison.Ordinal);
        var optimistic = commands.IndexOf("Phase = ScanPhase.Discovering;", reserve, StringComparison.Ordinal);
        var send = commands.IndexOf("await SendCommandAsync(new StartScanCommand", optimistic, StringComparison.Ordinal);
        var exactRelease = commands.IndexOf("_scanStartSlot.Release(owner)", send, StringComparison.Ordinal);
        var generationCheck = commands.IndexOf("owner.Generation == SpawnGeneration", exactRelease, StringComparison.Ordinal);
        var revisionCheck = commands.IndexOf("_scanPresentationRevision", generationCheck, StringComparison.Ordinal);
        Assert.True(start >= 0 && reserve > start && optimistic > reserve && send > optimistic,
            "StartScanAsync must reserve before publishing its optimistic phase and sending.");
        Assert.True(exactRelease > send && generationCheck > exactRelease && revisionCheck > generationCheck,
            "Send failure rollback must retain exact owner, generation, and revision authority.");
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        Assert.Contains("ObserveAuthoritativeScanEvent(generation);", client, StringComparison.Ordinal);
        Assert.Contains("RejectScanStartCommand(generation);", client, StringComparison.Ordinal);
        Assert.Contains("Phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan", commands, StringComparison.Ordinal);
        Assert.Contains("\"scan_already_running\" => true", commands, StringComparison.Ordinal);

        var sidebar = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarProcessingControl.xaml.cs"));
        Assert.DoesNotContain("SetOptimisticScanningPhase", sidebar, StringComparison.Ordinal);
    }

    [Fact]
    public void BulkWaitOwnershipRejectsOverlapAndRegistersUndoBeforeItsWaiter()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        var method = commands.IndexOf("public async Task<BulkActionResult> WaitForBulkActionResultAsync", StringComparison.Ordinal);
        var tryReserve = commands.IndexOf("gate.WaitAsync(TimeSpan.Zero, ct)", method, StringComparison.Ordinal);
        var reset = commands.IndexOf("LastBulkAction = null;", method, StringComparison.Ordinal);
        var undo = commands.IndexOf("sendRegistration = beforeSend?.Invoke();", reset, StringComparison.Ordinal);
        var waiter = commands.IndexOf("PropertyChanged += handler;", reset, StringComparison.Ordinal);
        var timeout = commands.IndexOf("catch (TimeoutException)", waiter, StringComparison.Ordinal);
        var lateRelease = commands.IndexOf("terminal.Task.ContinueWith", timeout, StringComparison.Ordinal);

        Assert.True(tryReserve > method, "An unresolved same-prefix operation must be rejected instead of queued.");
        Assert.True(reset > tryReserve && undo > reset && waiter > undo,
            "Undo must subscribe before the bulk waiter so a terminal cannot dispose it before capture.");
        Assert.True(timeout > waiter && lateRelease > timeout,
            "A timed-out prefix must remain reserved until a late terminal or engine transition.");
        Assert.Contains("catch (TimeoutException) when (sendCompleted)", commands, StringComparison.Ordinal);

        var cleanup = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Cleanup", "CleanupView.xaml.cs"));
        Assert.Contains("timeout: Timeout.InfiniteTimeSpan", cleanup, StringComparison.Ordinal);
    }

    [Fact]
    public void RestructureRetriesRepublishErrorsAndUndoErrorsClearGlobalState()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        foreach (var method in new[] { "PlanRestructureAsync", "ApplyRestructureAsync", "UndoRestructureAsync" })
        {
            var start = commands.IndexOf(method, StringComparison.Ordinal);
            var send = commands.IndexOf("SendCommandAsync", start, StringComparison.Ordinal);
            var clear = commands.IndexOf("LastError = null;", start, StringComparison.Ordinal);
            Assert.True(start >= 0 && clear > start && clear < send,
                $"{method} must clear the prior value-equal error before sending a retry.");
        }

        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var errorCase = client.IndexOf("case ErrorEvent e:", StringComparison.Ordinal);
        var undoKind = client.IndexOf("e.Error.Kind == \"undo_restructure\"", errorCase, StringComparison.Ordinal);
        var clearUndo = client.IndexOf("UndoRestructureInFlight = false;", undoKind, StringComparison.Ordinal);
        var nextCase = client.IndexOf("case LogEvent:", errorCase, StringComparison.Ordinal);
        Assert.True(undoKind > errorCase && clearUndo > undoKind && clearUndo < nextCase,
            "Undo terminal errors must clear process-global in-flight state even while the view is unloaded.");
    }

    [Fact]
    public void GpuRemovalBlocksAnotherScanUntilANewEngineGenerationIsReady()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        var sidebar = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarProcessingControl.xaml.cs"));
        var sidebarXaml = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarProcessingControl.xaml"));
        var deepAnalyze = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "DeepAnalyze", "DeepAnalyzeView.xaml.cs"));

        Assert.Contains("e.Error.Kind == \"gpu_device_removed\"", client, StringComparison.Ordinal);
        Assert.Contains("_gpuDeviceRemovedGeneration != generation", client, StringComparison.Ordinal);
        Assert.Contains("if (LastError?.Kind == \"gpu_device_removed\") LastError = null;", client, StringComparison.Ordinal);
        Assert.Contains("pc.Phase == ScanPhase.Failed && !GpuDeviceRemoved", client, StringComparison.Ordinal);
        Assert.Contains("if (GpuDeviceRemoved)", commands, StringComparison.Ordinal);
        Assert.Contains("!EngineClient.Instance.GpuDeviceRemoved", sidebar, StringComparison.Ordinal);
        Assert.Contains("Use Restart Engine here in the sidebar", sidebar, StringComparison.Ordinal);
        Assert.Contains("OnRestartEngineClicked", sidebar, StringComparison.Ordinal);
        Assert.Contains("x:Name=\"RestartEngineButton\"", sidebarXaml, StringComparison.Ordinal);
        Assert.Contains("await EngineClient.Instance.RestartAsync()", sidebar, StringComparison.Ordinal);
        Assert.DoesNotContain("GPU recovery restart failed: \" + ex", sidebar, StringComparison.Ordinal);
        Assert.Contains("if (EngineClient.Instance.GpuDeviceRemoved)", deepAnalyze, StringComparison.Ordinal);
        Assert.Contains("reason = EngineClient.GpuRestartRequiredMessage;", deepAnalyze, StringComparison.Ordinal);
        Assert.Contains("SyncDeepAnalyzeControls();", deepAnalyze, StringComparison.Ordinal);
        Assert.Contains("await ShowAlertAsync(\"Deep Analyze stopped\", ex.Message);", deepAnalyze, StringComparison.Ordinal);
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
