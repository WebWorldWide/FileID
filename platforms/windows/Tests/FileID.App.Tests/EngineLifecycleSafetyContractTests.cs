using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class EngineLifecycleSafetyContractTests
{
    private static readonly string RepoRoot = FindRepoRoot();

    [Theory]
    [InlineData(2)]
    [InlineData(3)]
    public void PersistedRestructureUndoRequiresAtLeastOneValidEntry(int version)
    {
        var path = Path.Combine(Path.GetTempPath(), $"fileid-undo-{Guid.NewGuid():N}.ndjson");
        var root = Path.Combine(Path.GetTempPath(), "FileID-library");
        try
        {
            File.WriteAllText(path, $"{{\"version\":{version},\"library_root\":{System.Text.Json.JsonSerializer.Serialize(root)}}}\n");
            Assert.Null(EngineClient.ReadPersistedRestructureUndoRoot(path));

            var identity = version == 3
                ? ",\"source_identity\":{\"volume\":1,\"file\":2}"
                : "";
            File.AppendAllText(
                path,
                $"{{\"file_id\":1,\"from\":\"a\",\"to\":\"b\"{identity}}}\n");
            Assert.Equal(root, EngineClient.ReadPersistedRestructureUndoRoot(path));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void PersistedRestructureUndoFallsBackToSingleOwnedPriorJournal()
    {
        var directory = Path.Combine(
            Path.GetTempPath(),
            $"fileid-undo-prior-{Guid.NewGuid():N}");
        var path = Path.Combine(directory, "restructure_undo.ndjson");
        var root = Path.Combine(Path.GetTempPath(), "FileID-library");
        Directory.CreateDirectory(directory);
        try
        {
            var header =
                $"{{\"version\":3,\"library_root\":{System.Text.Json.JsonSerializer.Serialize(root)}}}\n";
            File.WriteAllText(path, header);
            var prior = Path.Combine(
                directory,
                $".restructure_undo.ndjson.prior-{Guid.NewGuid():D}");
            File.WriteAllText(
                prior,
                header
                + "{\"file_id\":1,\"from\":\"a\",\"to\":\"b\","
                + "\"source_identity\":{\"volume\":1,\"file\":2}}\n");
            var priorUpdated = new DateTime(2026, 7, 29, 10, 30, 0, DateTimeKind.Utc);
            File.SetLastWriteTimeUtc(prior, priorUpdated);

            Assert.Equal(root, EngineClient.ReadPersistedRestructureUndoRoot(path));
            Assert.Equal(
                priorUpdated,
                EngineClient.ReadPersistedRestructureUndo(path)?.UpdatedUtc);

            File.Delete(path);
            Assert.Equal(
                priorUpdated,
                EngineClient.ReadPersistedRestructureUndo(path)?.UpdatedUtc);

            File.WriteAllText(
                Path.Combine(
                    directory,
                    $".restructure_undo.ndjson.prior-{Guid.NewGuid():D}"),
                header
                + "{\"file_id\":2,\"from\":\"c\",\"to\":\"d\","
                + "\"source_identity\":{\"volume\":1,\"file\":3}}\n");
            Assert.Null(EngineClient.ReadPersistedRestructureUndoRoot(path));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Theory]
    [InlineData(2)]
    [InlineData(3)]
    public void PersistedShortcutUndoAcceptsValidCommittedManifest(int version)
    {
        var directory = NewTempDirectory("fileid-shortcut-valid");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var token = Guid.NewGuid().ToString("D");
        try
        {
            WriteShortcutManifest(directory, root, token, version);

            var persisted = EngineClient.ReadPersistedShortcutUndo(directory);

            Assert.True(persisted.HasValue);
            Assert.Equal(root, persisted.Value.LibraryRoot);
            Assert.Equal(token, persisted.Value.Token);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoValidatesEveryCommittedEntry()
    {
        var directory = NewTempDirectory("fileid-shortcut-all-entries");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var token = Guid.NewGuid().ToString("D");
        try
        {
            var path = WriteShortcutManifest(directory, root, token, version: 3);
            var validEntry = File.ReadAllLines(path)[1];
            File.AppendAllText(path, validEntry + "\n");
            Assert.Equal(
                token,
                EngineClient.ReadPersistedShortcutUndo(directory)?.Token);

            WriteShortcutManifest(directory, root, token, version: 3);
            File.AppendAllText(path, "{\"file_id\":2}\n");
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));

            WriteShortcutManifest(directory, root, token, version: 3);
            File.AppendAllText(path, new string('x', 64 * 1024 + 1) + "\n");
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));

            WriteShortcutManifest(directory, root, token, version: 3);
            using (var stream = new FileStream(path, FileMode.Append, FileAccess.Write))
            {
                stream.Write(new byte[] { 0xff, (byte)'\n' });
            }
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));

            WriteShortcutManifest(directory, root, token, version: 3);
            File.AppendAllText(path, "{\"file_id\":");
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoRequiresCanonicalStagingLinkName()
    {
        var directory = NewTempDirectory("fileid-shortcut-staging-name");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var token = Guid.NewGuid().ToString("D");
        try
        {
            WriteShortcutManifest(
                directory,
                root,
                token,
                version: 3,
                stagingLinkName: "not-a-guid.link");
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));

            WriteShortcutManifest(
                directory,
                root,
                token,
                version: 3,
                stagingLinkName: Path.Combine(
                    "nested",
                    Guid.NewGuid().ToString("D") + ".link"));
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoDiscoversValidPendingV3Intent()
    {
        var directory = NewTempDirectory("fileid-shortcut-intent");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var token = Guid.NewGuid().ToString("D");
        try
        {
            WriteShortcutManifest(
                directory,
                root,
                token,
                version: 3,
                headerOnly: true,
                writeIntent: true);

            var persisted = EngineClient.ReadPersistedShortcutUndo(directory);

            Assert.True(persisted.HasValue);
            Assert.Equal(token, persisted.Value.Token);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
            if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoSkipsCorruptManifestAndKeepsValidToken()
    {
        var directory = NewTempDirectory("fileid-shortcut-mixed");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var validToken = Guid.NewGuid().ToString("D");
        try
        {
            WriteShortcutManifest(directory, root, validToken, version: 3);
            File.WriteAllText(
                Path.Combine(directory, Guid.NewGuid().ToString("D") + ".ndjson"),
                "{\"version\":3");
            File.WriteAllText(
                Path.Combine(directory, Guid.NewGuid().ToString("D") + ".ndjson"),
                new string('x', 64 * 1024 + 1) + "\n");
            File.WriteAllBytes(
                Path.Combine(directory, Guid.NewGuid().ToString("D") + ".ndjson"),
                new byte[] { 0xff, (byte)'\n' });
            var truncatedToken = Guid.NewGuid().ToString("D");
            var truncatedPath = WriteShortcutManifest(
                directory,
                root,
                truncatedToken,
                version: 2);
            File.WriteAllText(
                truncatedPath,
                File.ReadAllText(truncatedPath).TrimEnd('\n'));

            var persisted = EngineClient.ReadPersistedShortcutUndo(directory);

            Assert.True(persisted.HasValue);
            Assert.Equal(validToken, persisted.Value.Token);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoUsesLifoAndFailsClosedOnTimestampTie()
    {
        var directory = NewTempDirectory("fileid-shortcut-lifo");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var olderToken = Guid.NewGuid().ToString("D");
        var newerToken = Guid.NewGuid().ToString("D");
        try
        {
            var olderPath = WriteShortcutManifest(directory, root, olderToken, version: 2);
            var newerPath = WriteShortcutManifest(directory, root, newerToken, version: 2);
            var olderTime = new DateTime(2026, 7, 29, 12, 0, 0, DateTimeKind.Utc);
            var newerTime = olderTime.AddMinutes(1);
            File.SetLastWriteTimeUtc(olderPath, olderTime);
            File.SetLastWriteTimeUtc(newerPath, newerTime);

            Assert.Equal(
                newerToken,
                EngineClient.ReadPersistedShortcutUndo(directory)?.Token);
            Assert.Equal(
                olderToken,
                EngineClient.ReadPersistedShortcutUndo(directory, newerToken)?.Token);

            File.SetLastWriteTimeUtc(newerPath, olderTime);
            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoRejectsMissingSourceIdentityAndReparseAttributes()
    {
        var directory = NewTempDirectory("fileid-shortcut-identity");
        var root = Path.Combine(Path.GetTempPath(), $"FileID-library-{Guid.NewGuid():N}");
        var token = Guid.NewGuid().ToString("D");
        try
        {
            WriteShortcutManifest(
                directory,
                root,
                token,
                version: 3,
                includeSourceIdentity: false);

            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));
            Assert.False(EngineClient.IsRegularPersistedUndoFileAttributes(
                FileAttributes.ReparsePoint));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void PersistedShortcutUndoBoundsManifestEnumeration()
    {
        var directory = NewTempDirectory("fileid-shortcut-bound");
        try
        {
            for (var i = 0; i <= 1024; i++)
            {
                File.WriteAllText(
                    Path.Combine(directory, Guid.NewGuid().ToString("D") + ".ndjson"),
                    "");
            }

            Assert.Null(EngineClient.ReadPersistedShortcutUndo(directory));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void SuccessfulShortcutUndoRescansForNextPersistedToken()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));

        Assert.Contains(
            "RefreshPersistedRestructureUndo(UndoRestructureShortcutToken);",
            client,
            StringComparison.Ordinal);
    }

    [Fact]
    public async Task EngineStdoutFrameLimitCountsUtf8BytesNotUtf16Characters()
    {
        var payload = string.Concat(Enumerable.Repeat("é", 20)) + "\n";
        await using var stream = new MemoryStream(System.Text.Encoding.UTF8.GetBytes(payload));
        using var reader = new StreamReader(stream, System.Text.Encoding.UTF8);
        var framing = new EngineClient.StdoutFraming();

        var frame = await EngineClient.ReadBoundedFrameAsync(
            reader,
            framing,
            CancellationToken.None,
            maxFrameBytes: 32);

        Assert.Null(frame);
        Assert.True(framing.OversizeDropped);
    }

    [Fact]
    public async Task UnterminatedEngineStdoutFrameAlsoUsesTheUtf8ByteLimit()
    {
        var payload = string.Concat(Enumerable.Repeat("é", 20));
        await using var stream = new MemoryStream(System.Text.Encoding.UTF8.GetBytes(payload));
        using var reader = new StreamReader(stream, System.Text.Encoding.UTF8);
        var framing = new EngineClient.StdoutFraming();

        var frame = await EngineClient.ReadBoundedFrameAsync(
            reader,
            framing,
            CancellationToken.None,
            maxFrameBytes: 32);

        Assert.Null(frame);
        Assert.True(framing.OversizeDropped);
    }

    [Fact]
    public void StopTimeoutIsExplicitAndRestartFailsClosed()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));

        Assert.Contains("public async Task<bool> StopAndWaitForExitAsync", commands, StringComparison.Ordinal);
        Assert.Contains("return false;", commands, StringComparison.Ordinal);
        Assert.Contains("if (!await StopAndWaitForExitCoreAsync", commands, StringComparison.Ordinal);
        Assert.Contains("restart was aborted", commands, StringComparison.Ordinal);
        Assert.Contains("shouldRun: restartAfterLateExit", commands, StringComparison.Ordinal);
        Assert.Contains("ArmExpectedExitRestart(intent.Revision)", commands, StringComparison.Ordinal);
        Assert.Contains("Volatile.Read(ref _isStarting) == 0", commands, StringComparison.Ordinal);

        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        Assert.Contains("ResolveCurrentExpectedExitRestartRevision()", client, StringComparison.Ordinal);
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
    public void EngineStartWaitsForTheUnsolicitedReadyEvent()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var start = client.IndexOf("private async Task StartCoreAsync(", StringComparison.Ordinal);
        var end = client.IndexOf("private void TerminateSupersededStart", start, StringComparison.Ordinal);

        Assert.True(start >= 0 && end > start, "StartCoreAsync source region must remain discoverable.");
        Assert.DoesNotContain("new RequestStatusCommand()", client[start..end], StringComparison.Ordinal);
        Assert.Contains("_stdoutLoop = Task.Run(", client[start..end], StringComparison.Ordinal);
    }

    [Fact]
    public void StaleEngineEventsCannotMutateTheReplacementProcessState()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));

        Assert.Contains("startedProcess.StandardOutput", client, StringComparison.Ordinal);
        Assert.Contains("generation,", client, StringComparison.Ordinal);
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
        Assert.DoesNotContain("pc.Phase == ScanPhase.Failed && !GpuDeviceRemoved", client, StringComparison.Ordinal);
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

    [Fact]
    public void SignedEngineLeasePrecedesTrustVerificationAndSpansSpawn()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var pin = client.IndexOf("new FileStream(enginePath, FileMode.Open, FileAccess.Read, FileShare.Read)", StringComparison.Ordinal);
        var lease = client.IndexOf("using var spawnPinLease = spawnPin;", pin, StringComparison.Ordinal);
        var verify = client.IndexOf("var verdict = await Task.Run(() => WinVerifyTrustChecker.Verify(", lease, StringComparison.Ordinal);
        var spawn = client.IndexOf("startedProcess = Process.Start(psi)", verify, StringComparison.Ordinal);

        Assert.True(pin >= 0 && lease > pin && verify > lease && spawn > verify,
            "Signed builds must deny engine writes/deletes before trust verification and retain the lease through spawn.");
    }

    [Fact]
    public void ProgressDoesNotRetireScanControlRollbackOwnership()
    {
        var client = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.cs"));
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        var progress = client.IndexOf("case ProgressEvent p:", StringComparison.Ordinal);
        var phase = client.IndexOf("case PhaseChangedEvent pc:", progress, StringComparison.Ordinal);

        Assert.DoesNotContain("_scanControlRevision", client[progress..phase], StringComparison.Ordinal);
        Assert.Contains("Interlocked.Increment(ref _scanControlRevision)", commands, StringComparison.Ordinal);
        Assert.Contains("Interlocked.Read(ref _scanControlRevision) == revision", commands, StringComparison.Ordinal);
        Assert.Contains("await EnsureScanStartConfirmedAsync();", commands, StringComparison.Ordinal);
        Assert.Contains("var predecessor = _writeTail;", commands, StringComparison.Ordinal);
        Assert.Contains("_writeTail = queued;", commands, StringComparison.Ordinal);
        Assert.Contains("await predecessor.ConfigureAwait(false);", commands, StringComparison.Ordinal);
    }

    [Fact]
    public void OutboundQueueRejectsStaleEngineGenerationBeforeWriting()
    {
        var commands = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "ViewModels", "EngineClient.Commands.cs"));
        var capture = commands.IndexOf("var generation = SpawnGeneration;", StringComparison.Ordinal);
        var awaitPredecessor = commands.IndexOf("await predecessor.ConfigureAwait(false);", capture, StringComparison.Ordinal);
        var writeLock = commands.IndexOf("lock (_writeLock)", awaitPredecessor, StringComparison.Ordinal);
        var staleCheck = commands.IndexOf("if (generation != SpawnGeneration)", writeLock, StringComparison.Ordinal);
        var callback = commands.IndexOf("onWriteStarted?.Invoke();", staleCheck, StringComparison.Ordinal);
        var write = commands.IndexOf("_stdin.BaseStream.Write", callback, StringComparison.Ordinal);

        Assert.True(capture >= 0 && awaitPredecessor > capture && writeLock > awaitPredecessor,
            "Each queued command must retain the generation that owned it before waiting in the FIFO.");
        Assert.True(staleCheck > writeLock && callback > staleCheck && write > callback,
            "A stale generation must be rejected under the write lock before callbacks or bytes reach replacement stdin.");
    }

    [Fact]
    public void AutoScanAndQueueUseBoundedTerminalAndIncrementalContracts()
    {
        var app = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "App.xaml.cs"));
        var queue = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarQueueList.xaml.cs"));
        var queueXaml = File.ReadAllText(PathInRepo(
            "platforms", "windows", "src", "FileID.App", "Views", "Sidebar", "SidebarQueueList.xaml"));

        Assert.Contains("ScanPhase.Cancelled", app, StringComparison.Ordinal);
        Assert.Contains("LifecycleState.Crashed", app, StringComparison.Ordinal);
        Assert.Contains("SpawnGeneration != scanGeneration", app, StringComparison.Ordinal);
        Assert.Contains("var applyDrained = new TaskCompletionSource<bool>", app, StringComparison.Ordinal);
        Assert.Contains("applyDrained.TrySetResult(true)", app, StringComparison.Ordinal);
        Assert.Contains("faceTerminal!.Task.WaitAsync(TimeSpan.FromHours(12))", app, StringComparison.Ordinal);
        Assert.Contains("nameof(ViewModels.EngineClient.LastFaceClustering)", app, StringComparison.Ordinal);
        Assert.Contains("processed={EngineClient.Instance.LastScanProcessedFiles}", app, StringComparison.Ordinal);
        Assert.Contains("persons={faceResult?.PersonCount ?? 0}", app, StringComparison.Ordinal);
        var guiHarness = File.ReadAllText(PathInRepo(
            "platforms", "windows", "build", "gui-regression.ps1"));
        Assert.Contains("[string]$AppExecutable", guiHarness, StringComparison.Ordinal);
        Assert.Contains("-AppExecutable requires -SkipBuild", guiHarness, StringComparison.Ordinal);
        Assert.Contains("face clustering completed", guiHarness, StringComparison.Ordinal);
        Assert.Contains("JobsRepeater.ItemsSource = _visibleRows", queue, StringComparison.Ordinal);
        Assert.Contains("_visibleRows.Move(currentIndex, index)", queue, StringComparison.Ordinal);
        Assert.DoesNotContain("Children.Clear", queue, StringComparison.Ordinal);
        Assert.Contains("<StackLayout Spacing=\"4\" />", queueXaml, StringComparison.Ordinal);
        Assert.Contains("MaxHeight=\"280\"", queueXaml, StringComparison.Ordinal);
    }

    private static string NewTempDirectory(string prefix)
    {
        var directory = Path.Combine(
            Path.GetTempPath(),
            $"{prefix}-{Guid.NewGuid():N}");
        Directory.CreateDirectory(directory);
        return directory;
    }

    private static string WriteShortcutManifest(
        string directory,
        string root,
        string token,
        int version,
        bool includeSourceIdentity = true,
        bool headerOnly = false,
        bool writeIntent = false,
        string? stagingLinkName = null)
    {
        var stagingDirectory = Path.Combine(
            root,
            ".fileid-restructure-shortcut-staging",
            token);
        var identity = "{\"volume\":1,\"file\":2}";
        var header = version == 3
            ? $"{{\"version\":3,\"library_root\":{Json(root)},"
                + $"\"token\":{Json(token)},\"staging_dir\":{Json(stagingDirectory)},"
                + $"\"staging_dir_identity\":{identity}}}\n"
            : $"{{\"version\":2,\"library_root\":{Json(root)},"
                + $"\"token\":{Json(token)}}}\n";
        var path = Path.Combine(directory, token + ".ndjson");
        if (!headerOnly)
        {
            var entry = $"{{\"file_id\":1,\"source\":{Json(Path.Combine(root, "source.jpg"))},"
                + $"\"link\":{Json(Path.Combine(root, "Organized", "source.jpg"))},"
                + (version == 3
                    ? $"\"staging_link\":{Json(Path.Combine(
                        stagingDirectory,
                        stagingLinkName ?? Guid.NewGuid().ToString("D") + ".link"))},"
                    : "")
                + (includeSourceIdentity ? $"\"source_identity\":{identity}," : "")
                + $"\"link_identity\":{identity}}}\n";
            File.WriteAllText(path, header + entry);
            return path;
        }

        File.WriteAllText(path, header);
        if (writeIntent)
        {
            Directory.CreateDirectory(stagingDirectory);
            var operationId = Guid.NewGuid().ToString("D");
            var intent = $"{{\"version\":1,\"token\":{Json(token)},"
                + $"\"operation_id\":{Json(operationId)},\"file_id\":1,"
                + $"\"source\":{Json(Path.Combine(root, "source.jpg"))},"
                + $"\"link\":{Json(Path.Combine(root, "Organized", "source.jpg"))},"
                + $"\"staging_link\":{Json(Path.Combine(stagingDirectory, operationId + ".link"))},"
                + $"\"source_identity\":{identity}}}\n";
            File.WriteAllText(
                Path.Combine(stagingDirectory, operationId + ".intent.json"),
                intent);
        }
        return path;
    }

    private static string Json(string value)
        => System.Text.Json.JsonSerializer.Serialize(value);

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
