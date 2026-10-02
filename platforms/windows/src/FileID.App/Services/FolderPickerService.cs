// FolderPickerService — opens the standard Windows folder picker associated
// with the current window. Uses Windows.Storage.Pickers.FolderPicker, which
// internally bridges to IFileOpenDialog (the shell folder picker).
//
// Unpackaged WinUI 3 quirk: the picker MUST be associated with an HWND via
// WinRT.Interop.InitializeWithWindow.Initialize before PickSingleFolderAsync.
// Without it, the picker silently no-ops on unpackaged hosts.
//
//'s NSOpenPanel flow (canChooseDirectories,
// readability pre-check, alert on failure). The readability check returns
// false when the folder is on a network share that's offline, a volume the
// user doesn't have access to, or a redirected location that's broken.

using System.IO;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace FileID.Services;

internal static class FolderPickerService
{
    public sealed record PickResult(string? Path, string? FailureReason);

    public static async Task<PickResult> PickFolderAsync(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero)
        {
            return new PickResult(null, "Could not attach the folder picker to the app window.");
        }

        var picker = new FolderPicker
        {
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
        };
        // FolderPicker requires at least one FileTypeFilter entry on
        // unpackaged WinUI 3; "*" matches anything.
        picker.FileTypeFilter.Add("*");

        WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

        StorageFolder? folder;
        try
        {
            folder = await picker.PickSingleFolderAsync();
        }
        catch (Exception ex)
        {
            if (IsCancellationHResult(ex.HResult))
            {
                return new PickResult(null, null);
            }

            DebugLog.Warn("FolderPicker.PickSingleFolderAsync threw: " + ex.Message);
            return new PickResult(null, "The folder picker failed to open. Try again.");
        }

        if (folder is null)
        {
            // User cancelled — not an error.
            return new PickResult(null, null);
        }

        var result = ValidateSelectedPath(folder.Path);
        if (result.FailureReason is not null)
        {
            DebugLog.Warn($"FolderPicker rejected (not readable): {PathRedactor.Redact(folder.Path)} — {result.FailureReason}");
        }
        return result;
    }

    internal static bool IsCancellationHResult(int hresult) => hresult == unchecked((int)0x800704C7);

    internal static PickResult ValidateSelectedPath(string path) =>
        IsReadable(path, out string? reason)
            ? new PickResult(path, null)
            : new PickResult(null, reason ?? "FileID couldn't read the selected folder.");

    private static bool IsReadable(string path, out string? reason)
    {
        try
        {
            // Trying to enumerate the first entry is the most reliable check —
            // catches network shares that are unreachable, permissions denied,
            // and antivirus locks that say-yes-to-stat-but-no-to-open.
            using var enumerator = Directory.EnumerateFileSystemEntries(path).GetEnumerator();
            enumerator.MoveNext();
            reason = null;
            return true;
        }
        catch (UnauthorizedAccessException)
        {
            reason = "FileID doesn't have permission to read that folder. Pick a folder you own, or grant access in Properties → Security.";
            return false;
        }
        catch (DirectoryNotFoundException)
        {
            reason = "That folder no longer exists.";
            return false;
        }
        catch (IOException ex)
        {
            reason = "FileID couldn't open that folder: " + ex.Message;
            return false;
        }
        catch (Exception ex)
        {
            reason = "FileID couldn't read that folder: " + ex.Message;
            return false;
        }
    }
}
