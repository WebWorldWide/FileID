using FileID.IpcSchema;
using FileID.ViewModels;

namespace FileID.Services;

/// <summary>Runs the actions Chat proposes, only through paths the pages already use: engine
/// commands, Duplicates' byte-verified Recycle Bin path, People's edits and the suggested-name
/// rename. Nothing runs until the user presses the card's button; kinds that change files or
/// people only run from a card that showed what will happen (CHAT.md "Actions").</summary>
internal sealed class ChatActionRunner
{
    public static ChatActionRunner Instance { get; } = new();

    private readonly object _gate = new();
    private readonly Dictionary<string, ChatActionPhase> _phases = new(StringComparer.Ordinal);
    private readonly Dictionary<string, (string Label, Func<Task<string>> Undo)> _undos = new(StringComparer.Ordinal);
    private EngineClient Engine => EngineClient.Instance;

    /// <summary>Raised with the action id whenever its phase or Undo changes. May arrive off the UI thread.</summary>
    public event EventHandler<string>? Changed;

    public ChatActionPhase Phase(string id)
    {
        lock (_gate) return _phases.GetValueOrDefault(id) ?? ChatActionPhase.Proposed;
    }

    public string? UndoLabel(string id)
    {
        lock (_gate)
            return _phases.GetValueOrDefault(id)?.Stage == ChatActionStage.Done && _undos.TryGetValue(id, out var undo) ? undo.Label : null;
    }

    public bool CanStop(ChatAction action) => Phase(action.Id).Stage == ChatActionStage.Running && ChatActionPolicy.CanStop(action.Kind);

    public void Cancel(ChatAction action)
    {
        lock (_gate)
        {
            if (_phases.GetValueOrDefault(action.Id) is { Stage: not ChatActionStage.Proposed }) return;
            _phases[action.Id] = ChatActionPhase.Cancelled;
        }
        Raise(action.Id);
    }

    public async Task StopAsync(ChatAction action)
    {
        try
        {
            switch (action.Kind)
            {
                case "scanFolder" or "rescan":
                    await Engine.CancelScanAsync();
                    break;
                case "describeFiles":
                    await Engine.DeepAnalyzeCancelAsync();
                    break;
                default:
                    await Engine.SendCommandAsync(new CancelRestructureCommand());
                    break;
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Chat action stop failed: " + ex.Message);
        }
    }

    public async Task UndoAsync(ChatAction action)
    {
        Func<Task<string>> undo;
        lock (_gate)
        {
            if (_phases.GetValueOrDefault(action.Id)?.Stage != ChatActionStage.Done || !_undos.Remove(action.Id, out var entry)) return;
            undo = entry.Undo;
            _phases[action.Id] = ChatActionPhase.Running("Undoing…");
        }
        Raise(action.Id);
        try { Set(action.Id, ChatActionPhase.Done(await undo())); }
        catch (Exception ex) { Set(action.Id, ChatActionPhase.Failed("Undo didn't finish: " + SqliteErrorTranslator.Humanize(ex))); }
    }

    /// <summary>Runs <paramref name="action"/> once, from its card's button.</summary>
    public async Task RunAsync(ChatAction action)
    {
        if (!ChatActionPolicy.IsRunnable(action))
        {
            Set(action.Id, ChatActionPhase.Failed("This action isn't available."));
            return;
        }
        lock (_gate)
        {
            if (_phases.GetValueOrDefault(action.Id) is { Stage: not ChatActionStage.Proposed }) return;
            _phases[action.Id] = ChatActionPhase.Running("Starting");
        }
        Raise(action.Id);
        try
        {
            await RunCoreAsync(action);
        }
        catch (TimeoutException)
        {
            Fail(action.Id, "FileID didn't confirm this in time. Check the files before trying again.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Chat action {action.Kind} failed: {ex.Message}");
            Fail(action.Id, SqliteErrorTranslator.Humanize(ex));
        }
    }

    private async Task RunCoreAsync(ChatAction action)
    {
        var app = AppViewModel.Instance;
        switch (action.Kind)
        {
            case "scanFolder" or "rescan":
                await ScanAsync(action);
                break;
            case "describeFiles":
                await DescribeAsync(action);
                break;
            case "stopJob":
                await StopJobAsync(action.Job ?? "all");
                Set(action.Id, ChatActionPhase.Done("Asked FileID to stop. Finished work is kept."));
                break;
            case "findDuplicates" or "openPage":
                app.ActiveTab = SidebarTab.ById(ChatActionPolicy.TabId(action.Page ?? (action.Kind == "findDuplicates" ? "cleanup" : "library")));
                Set(action.Id, ChatActionPhase.Done($"Opened {ChatActionPolicy.PageTitle(action.Page ?? (action.Kind == "findDuplicates" ? "cleanup" : "library"))}."));
                break;
            case "search":
                app.RequestLibraryQuery(action.Query ?? "");
                Set(action.Id, ChatActionPhase.Done("Opened Library with this search."));
                break;
            case "findMoments":
                app.RequestMomentsQuery(action.Query ?? "");
                Set(action.Id, ChatActionPhase.Done("Opened Search & Moments with this search."));
                break;
            case "exportFiles":
                await ExportAsync(action);
                break;
            case "changeSetting":
                ChangeSetting(action);
                break;
            case "suggestFolderLayout":
                await SuggestLayoutAsync(action);
                break;
            case "applyFolderLayout":
                await ApplyLayoutAsync(action);
                break;
            case "undoLastMove":
                await UndoLastMoveAsync(action);
                break;
            case "moveDuplicatesToTrash":
                await TrashDuplicatesAsync(action);
                break;
            case "renameToSuggested":
                await RenameAsync(action);
                break;
            case "namePerson":
                await NamePersonAsync(action);
                break;
            case "mergePeople":
                await MergeAsync(action);
                break;
            default:
                Fail(action.Id, "This action isn't available.");
                break;
        }
    }

    private async Task ScanAsync(ChatAction action)
    {
        var app = AppViewModel.Instance;
        var path = action.Kind == "rescan" ? action.Path ?? app.FolderPath : action.Path;
        if (string.IsNullOrWhiteSpace(path))
        {
            Fail(action.Id, "Choose a folder first.");
            return;
        }
        var generation = Engine.SpawnGeneration;
        Set(action.Id, ChatActionPhase.Running("Starting the scan"));
        await Engine.StartScanAsync(path, HitPresentation.FileName(path.TrimEnd('\\', '/')) is { Length: > 0 } name ? name : path);
        if (action.Kind == "scanFolder") app.FolderPath = path;
        await WatchAsync(action.Id, generation,
            started: () => ChatActionPolicy.IsScanning(Engine.Phase),
            finished: () => !ChatActionPolicy.IsScanning(Engine.Phase),
            progress: () => ChatActionPolicy.ScanProgress(Engine.LastProgress, "Scanning"),
            result: () => ChatActionPolicy.ScanResult(Engine.Phase, Engine.Phase == ScanPhase.Completed ? Engine.LastScanProcessedFiles : Engine.LastProgress?.Processed ?? 0, Engine.LastProgress?.Failed ?? 0));
    }

    private async Task DescribeAsync(ChatAction action)
    {
        var model = AppViewModel.Instance.Settings.SelectedVlmModelKind;
        var generation = Engine.SpawnGeneration;
        if (action.FileIDs is { Count: > 0 } ids) await Engine.DescribeFilesAsync(ids, model);
        else await Engine.DeepAnalyzeAllAsync(model, skipExisting: true);
        await WatchAsync(action.Id, generation,
            started: () => Engine.DeepAnalyzeCommandInFlight || Engine.DeepAnalyzeProgress is not null,
            finished: () => !Engine.DeepAnalyzeCommandInFlight,
            progress: () => ChatActionPolicy.DescribeProgress(Engine.DeepAnalyzeProgress, Engine.DeepAnalyzeStarting is not null, "Starting"),
            result: () => ChatActionPolicy.DescribeResult(Engine.DeepAnalyzeComplete));
    }

    private async Task StopJobAsync(string job)
    {
        if (job is "scan" or "all" && ChatActionPolicy.IsScanning(Engine.Phase)) await Engine.CancelScanAsync();
        if (job is "describe" or "all" && (Engine.DeepAnalyzeCommandInFlight || Engine.DeepAnalyzeProgress is not null)) await Engine.DeepAnalyzeCancelAsync();
        if (job is "organize" or "all") await Engine.SendCommandAsync(new CancelRestructureCommand());
        if (job is not ("moments" or "all")) return;
        var jobs = await Engine.CatalogAsync(new CatalogRequest(Guid.NewGuid().ToString(), "jobs", null, null, null, null, null, null));
        foreach (var running in jobs.Jobs.Where(item => item.State is "running" or "queued"))
            await Engine.CatalogAsync(CatalogJobActions.ControlRequest("cancelJob", running));
    }

    private async Task ExportAsync(ChatAction action)
    {
        var rows = await ReadFilesAsync(action.FileIDs!);
        var hits = action.FileIDs!.Where(rows.ContainsKey).Distinct()
            .Select(id => new CatalogHit(id, rows[id].Path, rows[id].Kind, "", null, null, null)).ToArray();
        if (hits.Length == 0)
        {
            Fail(action.Id, "Those files are no longer in the library.");
            return;
        }
        AppViewModel.Instance.RequestToolFiles(hits);
        Set(action.Id, ChatActionPhase.Done($"Opened File Tools with {hits.Length:N0} files chosen. Pick a format there."));
    }

    private void ChangeSetting(ChatAction action)
    {
        if (!ChatActionPolicy.SettingSupported(action.Setting) || action.Value is not { } value)
        {
            Fail(action.Id, "This setting isn't on Windows yet.");
            return;
        }
        var settings = AppViewModel.Instance.Settings;
        var previous = settings.CleanupAutoTagKept;
        settings.CleanupAutoTagKept = value;
        settings.Save();
        Set(action.Id, ChatActionPhase.Done(value ? "Turned on." : "Turned off."));
        OfferUndo(action.Id, "Undo", () =>
        {
            settings.CleanupAutoTagKept = previous;
            settings.Save();
            return Task.FromResult("Changed back.");
        });
    }

    private async Task SuggestLayoutAsync(ChatAction action)
    {
        var path = action.Path ?? AppViewModel.Instance.FolderPath;
        if (string.IsNullOrWhiteSpace(path))
        {
            Fail(action.Id, "Choose a folder first.");
            return;
        }
        Set(action.Id, ChatActionPhase.Running("Planning a tidier layout"));
        var plan = await Engine.PlanRestructureAndWaitAsync(path);
        Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.PlanReady(ChatActionPolicy.Plan(plan, action))));
    }

    private async Task ApplyLayoutAsync(ChatAction action)
    {
        var summary = ChatActionPolicy.Plan(Engine.LastRestructurePlan, action);
        if (summary is null)
        {
            Fail(action.Id, "There's no suggested layout for this folder yet. Ask Chat to organize it first.");
            return;
        }
        if (summary.TooLarge)
        {
            Fail(action.Id, "This layout has more moves than Chat can apply on Windows. Review it in Organize Folders.");
            return;
        }
        Set(action.Id, ChatActionPhase.Running("Moving files"));
        var result = await Engine.ApplyRestructureAndWaitAsync(summary.Root, summary.Moves);
        Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.MoveResult(result)));
        if (result.Applied == 0 || !Engine.CanUndoRestructure) return;
        var root = summary.Root;
        OfferUndo(action.Id, "Undo moves", async () => ChatActionPolicy.MovedBack(await Engine.UndoRestructureAndWaitAsync(root)));
    }

    private async Task UndoLastMoveAsync(ChatAction action)
    {
        var path = action.Path ?? Engine.RestructureOperationRoot ?? AppViewModel.Instance.FolderPath;
        if (!Engine.CanUndoRestructure || string.IsNullOrWhiteSpace(path))
        {
            Fail(action.Id, "There's no applied layout to undo.");
            return;
        }
        Set(action.Id, ChatActionPhase.Running("Moving files back"));
        Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.MovedBack(await Engine.UndoRestructureAndWaitAsync(path))));
    }

    private async Task TrashDuplicatesAsync(ChatAction action)
    {
        var victims = action.FileIDs!;
        var keepers = action.KeeperFileIDs!;
        Set(action.Id, ChatActionPhase.Running("Checking copies"));
        var rows = await ReadFilesAsync(victims.Concat(keepers).ToArray());
        var missing = 0;
        var groups = new Dictionary<long, (ExactCleanupFile Keeper, List<ExactCleanupFile> Victims)>();
        foreach (var (victim, keeper) in victims.Zip(keepers))
        {
            if (!rows.TryGetValue(victim, out var file) || !rows.TryGetValue(keeper, out var kept))
            {
                missing++;
                continue;
            }
            if (!groups.TryGetValue(keeper, out var group))
                groups[keeper] = group = (new ExactCleanupFile(kept.Id, kept.Path, kept.SizeBytes), new List<ExactCleanupFile>());
            group.Victims.Add(new ExactCleanupFile(file.Id, file.Path, file.SizeBytes));
        }
        var requests = groups.Values.Select(group => new ExactCleanupGroupRequest(group.Keeper, group.Victims)).ToArray();
        var id = action.Id;
        var progress = new Progress<ExactCleanupProgress>(step => Set(id, ChatActionPhase.Running($"Checking copies · {step.CompletedFiles:N0} of {step.TotalFiles:N0}")));
        var proof = await ExactCleanupProofBuilder.BuildAsync(requests, progress, CancellationToken.None);
        var changed = proof.Rejections.Count + missing;
        if (proof.Identities.Count == 0)
        {
            Fail(action.Id, "Nothing was moved. Every copy changed since the scan or is no longer in the library; scan again first.");
            return;
        }
        Set(action.Id, ChatActionPhase.Running($"Moving {proof.Identities.Count:N0} copies to the Recycle Bin"));
        var requested = proof.Identities.Select(identity => identity.FileId).ToArray();
        var result = await Engine.WaitForBulkActionResultAsync(
            "trashFiles",
            () => Engine.TrashExactFilesAsync(proof.Identities),
            TimeSpan.FromMinutes(10));
        var confirmed = BulkActionResultTruth.ConfirmedSuccessfulFileIds(result, requested);
        var sizes = proof.Identities.ToDictionary(identity => identity.FileId, identity => identity.SizeBytes);
        var trashed = confirmed.Count > 0 ? confirmed.Count : (int)result.Succeeded;
        Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.TrashResult(trashed, confirmed.Sum(fileId => sizes.GetValueOrDefault(fileId)), changed, (int)result.Failed)));
        if (trashed == 0 || ChatActionPolicy.TrashBatch(result) is not { } batch) return;
        OfferUndo(action.Id, "Put them back", async () => ChatActionPolicy.RestoreResult(await Engine.RestoreFromTrashAsync(batch), trashed));
    }

    private async Task RenameAsync(ChatAction action)
    {
        var renames = action.Renames!;
        Set(action.Id, ChatActionPhase.Running($"Renaming {renames.Count:N0} files"));
        var rows = await ReadFilesAsync(renames.Select(rename => rename.FileID).ToArray());
        var plans = renames
            .DistinctBy(rename => rename.FileID)
            .Where(rename => rows.TryGetValue(rename.FileID, out var row) && ChatActionPolicy.SuggestionStillMatches(row.ProposedName, row.Path, rename.NewName))
            .ToArray();
        var skipped = renames.Count - plans.Length;
        if (plans.Length == 0)
        {
            Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.RenameResult(0, 0, skipped)));
            return;
        }
        var entries = plans.Select(rename => new RenameEntry(rename.FileID, rename.NewName)).ToArray();
        var result = await Engine.WaitForBulkActionResultAsync("renameFiles", () => Engine.RenameFilesAsync(entries), TimeSpan.FromSeconds(60));
        var confirmed = BulkActionResultTruth.ConfirmedSuccessfulFileIds(result, entries.Select(entry => entry.FileId)).ToHashSet();
        var inverse = plans.Where(rename => confirmed.Contains(rename.FileID))
            .Select(rename => new RenameEntry(rename.FileID, HitPresentation.FileName(rows[rename.FileID].Path))).ToArray();
        Set(action.Id, ChatActionPhase.Done(ChatActionPolicy.RenameResult(inverse.Length, (int)result.Failed, skipped)));
        if (inverse.Length == 0) return;
        OfferUndo(action.Id, "Undo renames", async () =>
        {
            var undone = await Engine.WaitForBulkActionResultAsync("renameFiles", () => Engine.RenameFilesAsync(inverse), TimeSpan.FromSeconds(60));
            return undone.Failed == 0
                ? $"Put back {undone.Succeeded:N0} names."
                : $"Put back {undone.Succeeded:N0} names · {undone.Failed:N0} couldn't be renamed.";
        });
    }

    private async Task NamePersonAsync(ChatAction action)
    {
        var personID = action.PersonID!.Value;
        var name = action.Name!.Trim();
        PersonNameRow? before;
        await using (var store = new ReadStore(AppPaths.DbPath))
        {
            await store.OpenAsync();
            before = await store.PersonNameAsync(personID, CancellationToken.None);
        }
        if (before is null)
        {
            Fail(action.Id, "That person is no longer in People.");
            return;
        }
        Set(action.Id, ChatActionPhase.Running("Saving the name"));
        var (first, last) = ChatActionPolicy.PersonName(name);
        var result = await Engine.WaitForBulkActionResultAsync("renamePerson",
            () => Engine.RenamePersonAsync(personID, null, first, null, last, null), TimeSpan.FromSeconds(30));
        if (result.Failed > 0 || result.Succeeded == 0)
        {
            Fail(action.Id, "The name wasn't saved. Try again in People.");
            return;
        }
        Set(action.Id, ChatActionPhase.Done($"Named this person {name}."));
        OfferUndo(action.Id, "Undo", async () =>
        {
            var undone = before.IsUnknown
                ? await Engine.WaitForBulkActionResultAsync("markPersonsAsUnknown", () => Engine.MarkPersonsAsUnknownAsync(new[] { personID }), TimeSpan.FromSeconds(30))
                : await Engine.WaitForBulkActionResultAsync("renamePerson", () => Engine.RenamePersonAsync(personID, before.Title,
                    before.Title is null && before.First is null && before.Middle is null && before.Last is null && before.Suffix is null ? before.Name : before.First,
                    before.Middle, before.Last, before.Suffix), TimeSpan.FromSeconds(30));
            return undone.Failed == 0 && undone.Succeeded > 0 ? "Name put back." : "The old name wasn't put back. Fix it in People.";
        });
    }

    private async Task MergeAsync(ChatAction action)
    {
        var target = action.PersonID!.Value;
        Set(action.Id, ChatActionPhase.Running("Merging"));
        var merged = 0;
        foreach (var source in action.PersonIDs!.Distinct())
        {
            var result = await Engine.WaitForBulkActionResultAsync("mergeClusters", () => Engine.MergeClustersAsync(source, target), TimeSpan.FromSeconds(30));
            if (result.Failed == 0 && result.Succeeded > 0) merged++;
        }
        if (merged == 0) Fail(action.Id, "The merge didn't go through. Nothing changed.");
        else Set(action.Id, ChatActionPhase.Done(merged == 1 ? "Merged 1 person into this one." : $"Merged {merged:N0} people into this one."));
    }

    private static async Task<IReadOnlyDictionary<long, ChatFileRow>> ReadFilesAsync(IReadOnlyCollection<long> ids)
    {
        await using var store = new ReadStore(AppPaths.DbPath);
        await store.OpenAsync();
        return await store.FilesByIdAsync(ids, CancellationToken.None);
    }

    /// <summary>Updates progress until <paramref name="finished"/>, then records the result. Gives a
    /// job up to 5 s to report that it started; an engine restart ends the wait.</summary>
    private async Task WatchAsync(string id, int generation, Func<bool> started, Func<bool> finished, Func<string> progress, Func<string> result)
    {
        var deadline = DateTime.UtcNow.AddSeconds(5);
        while (!started() && DateTime.UtcNow < deadline) await Task.Delay(200);
        while (!finished())
        {
            if (Engine.SpawnGeneration != generation)
            {
                Fail(id, "FileID's engine restarted before this finished. Check the page it belongs to, then try again.");
                return;
            }
            if (Phase(id).Stage != ChatActionStage.Running) return;
            Set(id, ChatActionPhase.Running(progress()));
            await Task.Delay(300);
        }
        if (Phase(id).Stage == ChatActionStage.Running) Set(id, ChatActionPhase.Done(result()));
    }

    private void OfferUndo(string id, string label, Func<Task<string>> undo)
    {
        lock (_gate) _undos[id] = (label, undo);
        Raise(id);
    }

    private void Fail(string id, string text) => Set(id, ChatActionPhase.Failed(text));

    private void Set(string id, ChatActionPhase phase)
    {
        lock (_gate)
        {
            if (_phases.GetValueOrDefault(id) == phase) return;
            _phases[id] = phase;
        }
        Raise(id);
    }

    private void Raise(string id)
    {
        if (Changed is not { } changed) return;
        foreach (var handler in changed.GetInvocationList().Cast<EventHandler<string>>())
        {
            try { handler(this, id); }
            catch (Exception ex) { DebugLog.Warn("Chat action listener threw: " + ex.Message); }
        }
    }
}
