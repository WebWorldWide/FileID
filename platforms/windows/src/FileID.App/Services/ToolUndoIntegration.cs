namespace FileID.Services;

internal static class ToolUndoIntegration
{
    // Enable only after the integrated engine passes no-clobber and cross-volume recovery acceptance.
    public static bool RecoverySafetyAccepted { get; } = true;
    public static string Detail => RecoverySafetyAccepted
        ? "Undo moves unchanged exports into internal recovery storage. Edited exports are preserved."
        : "Undo export is disabled pending integration and validation of no-clobber, cross-volume engine recovery. Export receipts remain available in Last export.";
    public static bool CanUndo(string? operationID) => RecoverySafetyAccepted && operationID is not null;
}
