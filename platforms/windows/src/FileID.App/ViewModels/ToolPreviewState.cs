using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class ToolPreviewState
{
    public string? PreviewID { get; private set; }
    public string? UndoID { get; private set; }
    public bool ExecuteAttempted { get; private set; }
    public bool CanExecute => PreviewID is not null && !ExecuteAttempted;

    public void Invalidate() => PreviewID = null;
    public void BeginExecute()
    {
        if (!CanExecute) throw new InvalidOperationException("Preview a fresh export before executing.");
        ExecuteAttempted = true;
    }

    public void Apply(string action, ToolResponse response)
    {
        if (action == "preview")
        {
            PreviewID = response.Status == "ok" ? response.OperationID : null;
            ExecuteAttempted = false;
        }
        if (action is "execute" or "history" or "undo")
        {
            PreviewID = null;
            if (response.Outputs.Any(output => output.State == "completed")) UndoID = response.OperationID ?? UndoID;
            else if (response.Status == "ok") UndoID = null;
        }
    }

    public static bool Supports(ToolCapability? capability, string format, uint dimension) =>
        capability is { Available: true, Id: "photo" or "chapters" or "video" } && capability.OutputFormats.Contains(format, StringComparer.Ordinal)
        && dimension is >= 1 and <= 8192;
}
