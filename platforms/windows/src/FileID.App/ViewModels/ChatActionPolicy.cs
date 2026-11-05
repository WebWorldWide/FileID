using System.Globalization;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal enum ChatActionStage { Proposed, Running, Done, Failed, Cancelled }

internal enum ChatActionAccent { Info, Gold, Danger }

internal sealed record ChatActionPhase(ChatActionStage Stage, string Text = "")
{
    public static ChatActionPhase Proposed { get; } = new(ChatActionStage.Proposed);
    public static ChatActionPhase Cancelled { get; } = new(ChatActionStage.Cancelled, "Cancelled. Nothing changed.");
    public static ChatActionPhase Running(string text) => new(ChatActionStage.Running, text);
    public static ChatActionPhase Done(string text) => new(ChatActionStage.Done, text);
    public static ChatActionPhase Failed(string text) => new(ChatActionStage.Failed, text);
}

/// <summary>What an applyFolderLayout card shows and sends: the Auto tier of the current plan.</summary>
internal sealed record ChatPlanSummary(string Root, int Count, IReadOnlyList<string> Examples, IReadOnlyList<RestructureMove> Moves, bool TooLarge);

/// <summary>Client copy of the engine's Chat action rules (CHAT.md "Actions"), so a reply
/// whose confirmation flag disagrees, whose kind is unknown or whose fields are missing
/// never shows a card and never runs.</summary>
internal static class ChatActionPolicy
{
    public const int MaxContextFiles = 10_000;
    public const int MaxDescribeSelection = 100;

    public static readonly IReadOnlySet<string> Kinds = new HashSet<string>(StringComparer.Ordinal)
    {
        "scanFolder", "rescan", "describeFiles", "stopJob", "findDuplicates", "moveDuplicatesToTrash",
        "suggestFolderLayout", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson",
        "mergePeople", "findMoments", "exportFiles", "openPage", "search", "changeSetting",
    };

    public static readonly IReadOnlySet<string> Confirming = new HashSet<string>(StringComparer.Ordinal)
    {
        "moveDuplicatesToTrash", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson", "mergePeople",
    };

    public static readonly IReadOnlySet<string> Pages = new HashSet<string>(StringComparer.Ordinal)
    {
        "library", "people", "cleanup", "deepAnalyze", "restructure", "settings", "search", "tools", "chat",
    };

    public static readonly IReadOnlySet<string> Settings = new HashSet<string>(StringComparer.Ordinal)
    {
        "cleanupAutoTag", "backgroundMotion", "chatSummaries",
    };

    private static readonly IReadOnlySet<string> Jobs = new HashSet<string>(StringComparer.Ordinal)
    {
        "scan", "describe", "organize", "moments", "all",
    };

    private static readonly IReadOnlySet<string> Stoppable = new HashSet<string>(StringComparer.Ordinal)
    {
        "scanFolder", "rescan", "describeFiles", "applyFolderLayout", "undoLastMove", "suggestFolderLayout",
    };

    public static bool IsRunnable(ChatAction action)
    {
        if (!Kinds.Contains(action.Kind)) return false;
        if (action.RequiresConfirmation != Confirming.Contains(action.Kind)) return false;
        if (string.IsNullOrWhiteSpace(action.Id) || string.IsNullOrWhiteSpace(action.Title)) return false;
        if (action.Page is { } page && !Pages.Contains(page)) return false;
        if (action.Setting is { } setting && !Settings.Contains(setting)) return false;
        if (action.Job is { } job && !Jobs.Contains(job)) return false;
        return action.Kind switch
        {
            "scanFolder" => !string.IsNullOrWhiteSpace(action.Path),
            "moveDuplicatesToTrash" => HasTrashPairs(action),
            "renameToSuggested" => action.Renames is { Count: > 0 } renames && renames.All(rename => rename.FileID > 0 && IsPlainName(rename.NewName)),
            "namePerson" => action.PersonID > 0 && !string.IsNullOrWhiteSpace(action.Name),
            "mergePeople" => action.PersonID is > 0 and var target && action.PersonIDs is { Count: > 0 } sources && sources.All(id => id > 0 && id != target),
            "changeSetting" => action.Setting is not null && action.Value is not null,
            "exportFiles" => action.FileIDs is { Count: > 0 },
            _ => true,
        };
    }

    public static IReadOnlyList<ChatAction> Runnable(IReadOnlyList<ChatAction>? actions)
    {
        if (actions is null || actions.Count == 0) return Array.Empty<ChatAction>();
        var seen = new HashSet<string>(StringComparer.Ordinal);
        return actions.Where(action => IsRunnable(action) && seen.Add(action.Id)).ToArray();
    }

    private static bool HasTrashPairs(ChatAction action) =>
        action.FileIDs is { Count: > 0 } victims
        && action.KeeperFileIDs is { } keepers
        && victims.Count == keepers.Count
        && victims.Distinct().Count() == victims.Count
        && victims.Zip(keepers).All(pair => pair.First > 0 && pair.Second > 0 && pair.First != pair.Second)
        && !victims.Intersect(keepers).Any();

    private static readonly System.Buffers.SearchValues<char> PathSeparators = System.Buffers.SearchValues.Create("/\\");

    private static bool IsPlainName(string name) =>
        !string.IsNullOrWhiteSpace(name) && name.AsSpan().IndexOfAny(PathSeparators) < 0 && name is not ("." or "..");

    public static string Label(ChatAction action) => action.RequiresConfirmation ? "Needs your OK" : "Suggested";

    public static ChatActionAccent Accent(ChatAction action) =>
        action.Kind == "moveDuplicatesToTrash" ? ChatActionAccent.Danger
        : action.RequiresConfirmation ? ChatActionAccent.Gold
        : ChatActionAccent.Info;

    public static bool IsDestructive(ChatAction action) => action.Kind == "moveDuplicatesToTrash";

    public static bool CanStop(string kind) => Stoppable.Contains(kind);

    public static string PageTitle(string? page) => page switch
    {
        "people" => "People",
        "cleanup" => "Duplicates",
        "deepAnalyze" => "Describe Files",
        "restructure" => "Organize Folders",
        "settings" => "Settings",
        "search" => "Search & Moments",
        "tools" => "File Tools",
        "chat" => "Chat",
        _ => "Library",
    };

    /// <summary>The WinUI page id (<see cref="SidebarTab"/>) for a ChatPage value.</summary>
    public static string TabId(string? page) => page switch
    {
        "people" => "people",
        "cleanup" => "cleanup",
        "deepAnalyze" => "deepanalyze",
        "restructure" => "restructure",
        "settings" => "settings",
        "search" => "moments",
        "tools" => "tools",
        "chat" => "chat",
        _ => "library",
    };

    public static ChatContext Context(IReadOnlyList<long>? selection, string? libraryRoot)
    {
        var ids = selection is { Count: > 0 } ? selection.Take(MaxContextFiles).ToArray() : null;
        var root = string.IsNullOrWhiteSpace(libraryRoot) ? null : libraryRoot;
        return new ChatContext("chat", ids, null, root, root);
    }

    public static string PrimaryTitle(ChatAction action, ChatPlanSummary? plan) =>
        action.Kind == "applyFolderLayout" && plan is { TooLarge: false }
            ? string.Create(CultureInfo.CurrentCulture, $"Move {plan.Count:N0} files")
            : action.Title;

    public static string Detail(ChatAction action, ChatPlanSummary? plan, bool canUndoMove)
    {
        switch (action.Kind)
        {
            case "applyFolderLayout" when plan is null:
                return "There's no suggested layout for this folder yet. Ask Chat to organize it first; nothing moves until you apply it.";
            case "applyFolderLayout" when plan.TooLarge:
                return "This layout has more moves than Chat can apply on Windows. Review it in Organize Folders.";
            case "applyFolderLayout":
                return string.Create(CultureInfo.CurrentCulture,
                    $"Moves {plan.Count:N0} files in {HitPresentation.FileName(plan.Root.TrimEnd('/', '\\'))} into the suggested folders. Files the plan marked for review stay where they are.");
            case "undoLastMove" when !canUndoMove:
                return "There's no applied layout to undo.";
            default:
                return action.Detail;
        }
    }

    public static bool IsUnavailable(ChatAction action, ChatPlanSummary? plan, bool canUndoMove) =>
        (action.Kind == "applyFolderLayout" && plan is null or { TooLarge: true })
        || (action.Kind == "undoLastMove" && !canUndoMove)
        || (action.Kind == "changeSetting" && !SettingSupported(action.Setting));

    public static long? FileCount(ChatAction action, ChatPlanSummary? plan) => plan is { TooLarge: false } ? plan.Count : action.FileCount;

    public static IReadOnlyList<string> Examples(ChatAction action, ChatPlanSummary? plan)
    {
        var source = plan is { TooLarge: false } ? plan.Examples : action.Examples ?? Array.Empty<string>();
        return source.Take(3).Select(ExampleName).ToArray();
    }

    /// <summary>A full path shows as its file name; other examples stay as written.</summary>
    public static string ExampleName(string example) => IsRooted(example) ? HitPresentation.FileName(example) : example;

    private static bool IsRooted(string value) =>
        value.StartsWith('/') || value.StartsWith('\\') || (value.Length >= 3 && char.IsAsciiLetter(value[0]) && value[1] == ':' && value[2] is '\\' or '/');

    public static long MoreCount(ChatAction action, ChatPlanSummary? plan)
    {
        var shown = Examples(action, plan).Count;
        var total = FileCount(action, plan) ?? 0;
        return shown > 0 && total > shown ? total - shown : 0;
    }

    public static string MiddleTruncate(string text, int max = 56)
    {
        if (text.Length <= max || max < 5) return text;
        var head = (max - 1) / 2;
        var tail = max - 1 - head;
        return string.Concat(text.AsSpan(0, head), "…", text.AsSpan(text.Length - tail));
    }

    public static ChatPlanSummary? Plan(RestructurePlan? plan, ChatAction action)
    {
        if (plan is null) return null;
        if (action.Path is { } path && !SamePath(path, plan.LibraryRoot)) return null;
        if (plan.Truncated)
        {
            var total = plan.ConfidenceCounts?.Auto ?? 0;
            return total > 0 ? new ChatPlanSummary(plan.LibraryRoot, (int)Math.Min(total, int.MaxValue), Array.Empty<string>(), Array.Empty<RestructureMove>(), TooLarge: true) : null;
        }
        var auto = plan.Moves.Where(move => move.Confidence == "auto").ToArray();
        if (auto.Length == 0) return null;
        var examples = auto.Take(3).Select(move =>
        {
            var folder = HitPresentation.Folder(move.Destination);
            var relative = folder.StartsWith(plan.LibraryRoot, StringComparison.OrdinalIgnoreCase)
                ? folder[plan.LibraryRoot.Length..].Trim('/', '\\')
                : folder;
            return $"{HitPresentation.FileName(move.Source)} → {(relative.Length == 0 ? "top folder" : relative)}";
        }).ToArray();
        return new ChatPlanSummary(plan.LibraryRoot, auto.Length, examples, auto, TooLarge: false);
    }

    public static bool SamePath(string a, string b) =>
        string.Equals(a.TrimEnd('/', '\\'), b.TrimEnd('/', '\\'), StringComparison.OrdinalIgnoreCase);

    public static (string? First, string? Last) PersonName(string name)
    {
        var parts = name.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (parts.Length == 0) return (null, null);
        return (parts[0], parts.Length > 1 ? string.Join(' ', parts.Skip(1)) : null);
    }

    /// <summary>Only Duplicates' "tag kept files" exists on Windows; the other Chat settings have no WinUI control.</summary>
    public static bool SettingSupported(string? setting) => setting == "cleanupAutoTag";

    /// <summary>A rename still applies only when the file's suggestion is the one the card showed.</summary>
    public static bool SuggestionStillMatches(string? proposedName, string currentPath, string newName)
    {
        var proposed = proposedName?.Trim();
        if (string.IsNullOrEmpty(proposed)) return false;
        var name = HitPresentation.FileName(currentPath);
        var dot = name.LastIndexOf('.');
        var extension = dot > 0 ? name[(dot + 1)..] : "";
        var expected = extension.Length == 0 ? proposed : $"{proposed}.{extension}";
        return string.Equals(expected, newName, StringComparison.Ordinal);
    }

    public static string ScanProgress(ScanProgress? progress, string fallback)
    {
        if (progress is null) return fallback;
        return progress.Phase switch
        {
            ScanPhase.Discovering => string.Create(CultureInfo.CurrentCulture, $"Finding files · {progress.Discovered:N0} so far"),
            ScanPhase.PostScan => "Finishing up",
            ScanPhase.Tagging => string.Create(CultureInfo.CurrentCulture, $"Scanning · {progress.Processed:N0} of {progress.Total:N0} files"),
            _ => fallback,
        };
    }

    public static bool IsScanning(ScanPhase? phase) => phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;

    public static string ScanResult(ScanPhase? phase, ulong processed, ulong failed) => phase switch
    {
        ScanPhase.Cancelled => "Scan stopped. Files found so far stay in the library.",
        ScanPhase.Failed => "The scan didn't finish. Scan again to pick up where it left off.",
        _ => string.Create(CultureInfo.CurrentCulture, $"Scan finished · {processed:N0} files{(failed > 0 ? $" · {failed:N0} couldn't be read" : "")}."),
    };

    public static string DescribeProgress(DeepAnalyzeProgress? progress, bool starting, string fallback) =>
        progress is not null
            ? string.Create(CultureInfo.CurrentCulture, $"Describing · {progress.Processed:N0} of {progress.Total:N0} files")
            : starting ? "Loading the local AI model" : fallback;

    public static string DescribeResult(DeepAnalyzeComplete? done)
    {
        if (done is null) return "Describing finished.";
        if (done.Cancelled) return string.Create(CultureInfo.CurrentCulture, $"Stopped after {done.Processed:N0} files. Their descriptions are kept.");
        return string.Create(CultureInfo.CurrentCulture, $"Described {done.Processed:N0} files{(done.Failed > 0 ? $" · {done.Failed:N0} couldn't be described" : "")}.");
    }

    public static string MoveResult(RestructureApplyResult result)
    {
        if (result.Cancelled) return string.Create(CultureInfo.CurrentCulture, $"Stopped after moving {result.Applied:N0} files.");
        return string.Create(CultureInfo.CurrentCulture, $"Moved {result.Applied:N0} files{(result.Failed > 0 ? $" · {result.Failed:N0} couldn't move" : "")}.");
    }

    public static string MovedBack(RestructureApplyResult result) =>
        string.Create(CultureInfo.CurrentCulture, $"Moved {result.Applied:N0} files back{(result.Failed > 0 ? $" · {result.Failed:N0} couldn't move" : "")}.");

    public static string PlanReady(ChatPlanSummary? summary) =>
        summary is null
            ? "Nothing needs to move."
            : string.Create(CultureInfo.CurrentCulture, $"Layout ready: {summary.Count:N0} files would move. Nothing has moved yet; review it in Organize Folders or ask Chat to apply it.");

    public static string TrashResult(int trashed, long freedBytes, int changed, int failed)
    {
        var text = string.Create(CultureInfo.CurrentCulture, $"Moved {trashed:N0} {(trashed == 1 ? "copy" : "copies")} to the Recycle Bin · freed {ByteFormat.File(freedBytes)}");
        if (changed > 0) text += string.Create(CultureInfo.CurrentCulture, $" · {changed:N0} skipped because the file changed since the scan");
        if (failed > 0) text += string.Create(CultureInfo.CurrentCulture, $" · {failed:N0} couldn't be moved");
        return text + ".";
    }

    public static string RestoreResult(bool restored, int count) =>
        restored
            ? string.Create(CultureInfo.CurrentCulture, $"Put back {count:N0} {(count == 1 ? "copy" : "copies")}.")
            : "Not everything was put back. Check the Recycle Bin.";

    public static string RenameResult(int renamed, int failed, int skipped)
    {
        var text = string.Create(CultureInfo.CurrentCulture, $"Renamed {renamed:N0} files");
        if (failed > 0) text += string.Create(CultureInfo.CurrentCulture, $" · {failed:N0} failed");
        if (skipped > 0) text += string.Create(CultureInfo.CurrentCulture, $" · {skipped:N0} skipped because their suggestion changed");
        return text + ".";
    }

    /// <summary>The batch id the engine appends to a trashFiles result ("trashFiles:&lt;batch&gt;").</summary>
    public static string? TrashBatch(BulkActionResult result)
    {
        var separator = result.Action.IndexOf(':');
        if (separator < 0 || !result.Action.AsSpan(0, separator).SequenceEqual("trashFiles")) return null;
        var batch = result.Action[(separator + 1)..];
        return batch.Length == 0 ? null : batch;
    }
}
