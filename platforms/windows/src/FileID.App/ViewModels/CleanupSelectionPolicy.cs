using System.Linq;

namespace FileID.ViewModels;

internal static class CleanupSelectionPolicy
{
    internal static DuplicateMember[] SelectedVictims(DuplicateGroup group)
        => group.IsSimilar
            ? group.Members.Where(member => member.IsSelectedForTrash).ToArray()
            : group.Members.Where(member => !member.IsKeeper).ToArray();

    internal static DuplicateMember? RetainedCopy(DuplicateGroup group)
        => group.IsSimilar
            ? group.Members.FirstOrDefault(member => !member.IsSelectedForTrash)
            : group.Members.Count(member => member.IsKeeper) == 1
                ? group.Members.First(member => member.IsKeeper)
                : null;
}
