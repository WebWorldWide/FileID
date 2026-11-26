// Opens the Windows folder picker. On unpackaged WinUI 3 the picker must be bound to the window HWND first or it silently does nothing.

using System.IO;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace FileID.Services;

internal static class FolderPickerService
{
    public sealed record PickResult(string? Path, string? FailureReason);

    public const string ScanCommitText = "Scan This Folder";

    public static async Task<PickResult> PickFolderAsync(IntPtr hwnd, string? commitButtonText = null)
    {
        if (hwnd == IntPtr.Zero)
        {
            return new PickResult(null, "Could not attach the folder picker to the app window.");
        }

        var picker = new FolderPicker
        {
            SuggestedStartLocation = PickerLocationId.PicturesLibrary,
        };
        if (commitButtonText is not null) picker.CommitButtonText = commitButtonText;
        // Unpackaged WinUI 3 requires at least one filter entry.
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
            // Enumerating the first entry catches unreachable network shares, denied permissions and antivirus locks that allow stat but not open.
            using var enumerator = Directory.EnumerateFileSystemEntries(path).GetEnumerator();
            enumerator.MoveNext();
            reason = null;
            return true;
        }
        catch (UnauthorizedAccessException)
        {
            reason = "FileID doesn't have permission to read that folder. Choose a different folder, or give FileID access in Properties → Security.";
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
