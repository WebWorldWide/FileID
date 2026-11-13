using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Wording and reply handling shared by every way of combining two
/// people cards.</summary>
internal static class PersonMergePolicy
{
    public const string ConfirmText = "Merge these people? You can undo this right after.";
    public const string UndoHint = "You can undo this right after.";
    public const string UndoLabel = "Merge people";
    public const string Merged = "Merged.";
    public const string PutBack = "Put back as two people.";
    public const string CouldNotMerge = "FileID couldn't merge these people. Try again in a moment.";
    public const string CouldNotPutBack = "FileID couldn't put them back. Open Recent changes to try again.";

    public static bool Succeeded(BulkActionResult result) => result.Failed == 0 && result.Succeeded > 0;

    /// <summary>The id of the helper's own record of this merge, when it sent
    /// one. Undo then replays that record instead of rebuilding the people
    /// from a list of faces.</summary>
    public static string? OperationId(BulkActionResult result)
    {
        foreach (var item in result.Messages ?? Array.Empty<BulkActionItem>())
        {
            if (item is { Ok: true, OperationId: { } id } && !string.IsNullOrWhiteSpace(id)) return id;
        }
        return null;
    }

    public static string FailureText(BulkActionResult result)
    {
        foreach (var item in result.Messages ?? Array.Empty<BulkActionItem>())
        {
            if (item is { Ok: false, Message: { } message } && !string.IsNullOrWhiteSpace(message)) return message.Trim();
        }
        return CouldNotMerge;
    }
}
