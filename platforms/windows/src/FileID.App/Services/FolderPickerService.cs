using System.IO;
using System.Runtime.InteropServices;

namespace FileID.Services;

internal static class FolderPickerService
{
    private const int ErrorCancelledHresult = unchecked((int)0x800704C7);
    private const uint ClsctxInprocServer = 0x1;

    private static readonly Guid FileOpenDialogClsid = new("DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7");
    private static readonly Guid FileDialogIid = new("42F85136-DB7E-439C-85F1-E4075D135FC8");
    private static readonly Guid ShellItemIid = new("43826D1E-E718-42EE-BC55-A1E261C37BFE");
    private static readonly Guid FileIdPickerClientGuid = new("E44D13D6-8E62-4EA8-96A5-8D0CD73039CE");

    public sealed record PickResult(string? Path, string? FailureReason);

    public static Task<PickResult> PickFolderAsync(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero || !IsWindow(hwnd))
        {
            DebugLog.Warn("Folder picker refused an invalid owner window handle.");
            return Task.FromResult(new PickResult(null,
                "FileID couldn't attach the folder picker to its window. Close and reopen FileID, then try again."));
        }

        try
        {
            var path = PickFolder(hwnd);
            return Task.FromResult(path is null
                ? new PickResult(null, null)
                : ValidateSelectedPath(path));
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Native folder picker failed: {ex.GetType().Name} HRESULT=0x{ex.HResult:X8}: {ex}");
            return Task.FromResult(new PickResult(null,
                "The Windows folder picker failed to open. Close and reopen FileID, then try again."));
        }
    }

    internal static bool IsCancellationHResult(int hresult)
        => hresult == ErrorCancelledHresult;

    internal static PickResult ValidateSelectedPath(string path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return new PickResult(null, "Windows didn't return a usable folder path. Pick a local or network folder and try again.");
        }

        if (!IsReadable(path, out string? reason))
        {
            DebugLog.Warn($"FolderPicker rejected (not readable): {PathRedactor.Redact(path)} — {reason}");
            return new PickResult(null, reason ?? "FileID couldn't read the selected folder.");
        }
        return new PickResult(path, null);
    }

    private static string? PickFolder(IntPtr hwnd)
    {
        IFileDialog? dialog = null;
        IShellItem? result = null;

        try
        {
            var clsid = FileOpenDialogClsid;
            var iid = FileDialogIid;
            ThrowIfFailed(CoCreateInstance(ref clsid, IntPtr.Zero, ClsctxInprocServer, ref iid, out dialog));

            ThrowIfFailed(dialog.GetOptions(out var options));
            options |= FileOpenOptions.PickFolders
                | FileOpenOptions.ForceFileSystem
                | FileOpenOptions.PathMustExist
                | FileOpenOptions.NoChangeDirectory
                | FileOpenOptions.DontAddToRecent;
            ThrowIfFailed(dialog.SetOptions(options));

            var clientGuid = FileIdPickerClientGuid;
            _ = dialog.SetClientGuid(ref clientGuid);
            _ = dialog.SetTitle("Choose a library folder");
            _ = dialog.SetOkButtonLabel("Choose folder");
            SetDefaultFolder(dialog);

            var showResult = dialog.Show(hwnd);
            if (IsCancellationHResult(showResult)) return null;
            ThrowIfFailed(showResult);

            ThrowIfFailed(dialog.GetResult(out result));
            ThrowIfFailed(result.GetDisplayName(ShellDisplayName.FileSystemPath, out var pathPointer));
            try
            {
                return Marshal.PtrToStringUni(pathPointer);
            }
            finally
            {
                Marshal.FreeCoTaskMem(pathPointer);
            }
        }
        finally
        {
            ReleaseComObject(result);
            ReleaseComObject(dialog);
        }
    }

    private static void SetDefaultFolder(IFileDialog dialog)
    {
        var pictures = Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);
        if (string.IsNullOrWhiteSpace(pictures) || !Directory.Exists(pictures)) return;

        IShellItem? folder = null;
        try
        {
            var iid = ShellItemIid;
            if (SHCreateItemFromParsingName(pictures, IntPtr.Zero, ref iid, out folder) >= 0)
            {
                _ = dialog.SetDefaultFolder(folder);
            }
        }
        finally
        {
            ReleaseComObject(folder);
        }
    }

    private static void ThrowIfFailed(int hresult)
    {
        if (hresult < 0) Marshal.ThrowExceptionForHR(hresult);
    }

    private static void ReleaseComObject(object? value)
    {
        if (value is not null && Marshal.IsComObject(value))
        {
            _ = Marshal.FinalReleaseComObject(value);
        }
    }

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
        catch (IOException)
        {
            reason = "FileID couldn't open that folder. Reconnect the drive or network share, then try again.";
            return false;
        }
        catch (Exception)
        {
            reason = "FileID couldn't read that folder. Pick a local or network folder you can open in File Explorer.";
            return false;
        }
    }

    [Flags]
    private enum FileOpenOptions : uint
    {
        NoChangeDirectory = 0x00000008,
        PickFolders = 0x00000020,
        ForceFileSystem = 0x00000040,
        PathMustExist = 0x00000800,
        DontAddToRecent = 0x02000000,
    }

    private enum ShellDisplayName : uint
    {
        FileSystemPath = 0x80058000,
    }

    [ComImport]
    [Guid("42F85136-DB7E-439C-85F1-E4075D135FC8")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IFileDialog
    {
        [PreserveSig] int Show(IntPtr parent);
        [PreserveSig] int SetFileTypes(uint count, IntPtr filterSpecs);
        [PreserveSig] int SetFileTypeIndex(uint fileType);
        [PreserveSig] int GetFileTypeIndex(out uint fileType);
        [PreserveSig] int Advise(IntPtr events, out uint cookie);
        [PreserveSig] int Unadvise(uint cookie);
        [PreserveSig] int SetOptions(FileOpenOptions options);
        [PreserveSig] int GetOptions(out FileOpenOptions options);
        [PreserveSig] int SetDefaultFolder(IShellItem folder);
        [PreserveSig] int SetFolder(IShellItem folder);
        [PreserveSig] int GetFolder(out IShellItem folder);
        [PreserveSig] int GetCurrentSelection(out IShellItem item);
        [PreserveSig] int SetFileName([MarshalAs(UnmanagedType.LPWStr)] string name);
        [PreserveSig] int GetFileName(out IntPtr name);
        [PreserveSig] int SetTitle([MarshalAs(UnmanagedType.LPWStr)] string title);
        [PreserveSig] int SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string label);
        [PreserveSig] int SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string label);
        [PreserveSig] int GetResult(out IShellItem item);
        [PreserveSig] int AddPlace(IShellItem item, uint alignment);
        [PreserveSig] int SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string extension);
        [PreserveSig] int Close(int hresult);
        [PreserveSig] int SetClientGuid(ref Guid clientGuid);
        [PreserveSig] int ClearClientData();
        [PreserveSig] int SetFilter(IntPtr filter);
    }

    [ComImport]
    [Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        [PreserveSig] int BindToHandler(IntPtr bindContext, ref Guid handler, ref Guid iid, out IntPtr result);
        [PreserveSig] int GetParent(out IShellItem parent);
        [PreserveSig] int GetDisplayName(ShellDisplayName displayName, out IntPtr name);
        [PreserveSig] int GetAttributes(uint mask, out uint attributes);
        [PreserveSig] int Compare(IShellItem item, uint hint, out int order);
    }

    [DllImport("ole32.dll", ExactSpelling = true)]
    [PreserveSig]
    private static extern int CoCreateInstance(
        ref Guid classId,
        IntPtr outer,
        uint classContext,
        ref Guid interfaceId,
        [MarshalAs(UnmanagedType.Interface)] out IFileDialog dialog);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    [PreserveSig]
    private static extern int SHCreateItemFromParsingName(
        [MarshalAs(UnmanagedType.LPWStr)] string path,
        IntPtr bindContext,
        ref Guid interfaceId,
        [MarshalAs(UnmanagedType.Interface)] out IShellItem item);

    [DllImport("user32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindow(IntPtr hwnd);
}
