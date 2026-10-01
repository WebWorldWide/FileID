using System.Collections.Generic;
using System.Linq;

namespace FileID.Services;

internal static class RestructureUndoPolicy
{
    internal static bool CanStart(bool canUndoForRoot, bool changeLogUndoInFlight, bool engineUndoInFlight)
        => canUndoForRoot && !changeLogUndoInFlight && !engineUndoInFlight;

    internal static bool ShouldRecord(bool appliedAsShortcuts, uint applied, bool canUndoThisRun)
        => !appliedAsShortcuts && applied > 0 && canUndoThisRun;

    internal static ChangeLogEntry? FindLatest(IEnumerable<ChangeLogEntry> entries)
        => entries.FirstOrDefault(entry => entry.Kind == ChangeKind.Restructure
            && entry.Status is ChangeStatus.Undoable or ChangeStatus.UndoFailed);
}
