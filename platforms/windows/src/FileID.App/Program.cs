// Replaces the XAML-generated Main (DISABLE_XAML_GENERATED_MAIN) to run the Windows App SDK
// bootstrapper before XAML starts and to gate on a single instance.

using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
#if !FILEID_STORE
using Microsoft.Windows.ApplicationModel.DynamicDependency;
#endif
using System.Threading;
using System.Security.Cryptography;
using System.Text;
using WinRT;

namespace FileID;

internal static class Program
{
    /// <summary>
    /// Single-instance mutex name. Per-session (Local\), not per-machine: a Global\ mutex would stop a
    /// second logged-in user (Fast User Switching, RDP) from launching their own copy.
    /// </summary>
    private const string SingleInstanceMutexName = "Local\\FileID-Singleton-{8C9D7C2E-3B87-4F19-8F3F-5A1A1B5E8A8E}";

    /// <summary>Folder from <c>--auto-scan-folder</c>; App.OnLaunched scans it once the engine is ready (used by build/gui-regression.ps1).</summary>
    public static string? AutoScanFolder { get; private set; }

    /// <summary>Set by <c>--auto-exit-after-scan</c>: close the main window once the auto-scan ends.</summary>
    public static bool AutoExitAfterScan { get; private set; }

    internal static string ResolveInstanceMutexName(string? testInstance, string? dbPath, string? localAppData)
    {
        if (string.IsNullOrWhiteSpace(testInstance)) return SingleInstanceMutexName;
        if (string.IsNullOrWhiteSpace(dbPath) || string.IsNullOrWhiteSpace(localAppData))
            throw new InvalidOperationException("A test instance requires isolated database and app data paths.");
        var identity = Encoding.UTF8.GetBytes($"{testInstance}\0{dbPath}\0{localAppData}");
        return "Local\\FileID-Test-" + Convert.ToHexString(SHA256.HashData(identity))[..24];
    }

    [STAThread]
    private static int Main(string[] args)
    {
        // A --auto-scan-folder flag without a value is ignored.
        for (int i = 0; i < args.Length; i++)
        {
            if (args[i] == "--auto-scan-folder" && i + 1 < args.Length && !string.IsNullOrWhiteSpace(args[i + 1]))
            {
                AutoScanFolder = args[i + 1];
            }
            else if (args[i] == "--auto-exit-after-scan")
            {
                AutoExitAfterScan = true;
            }
        }

        // Held for the process lifetime.
        using var instanceMutex = new Mutex(initiallyOwned: true, name: ResolveInstanceMutexName(Environment.GetEnvironmentVariable("FILEID_TEST_INSTANCE"), Environment.GetEnvironmentVariable("FILEID_DB"), Environment.GetEnvironmentVariable("LOCALAPPDATA")), out bool createdNew);
        if (!createdNew)
        {
            // Bring the running instance's window to the front and exit.
            try
            {
                var current = System.Diagnostics.Process.GetCurrentProcess();
                foreach (var p in System.Diagnostics.Process.GetProcessesByName("FileID"))
                {
                    if (p.Id != current.Id)
                    {
                        FocusProcessWindow(p.Id);
                    }
                }
            }
            catch { }
            return 0;
        }

        // Unpackaged apps need the bootstrapper before any XAML call. The version must match the
        // WinAppSDK pin in Directory.Packages.props (1.7 = 0x00010007).
#if !FILEID_STORE
        if (!Bootstrap.TryInitialize(0x00010007u, out int hr))
        {
            ShowFatalDialog(
                "Windows App SDK runtime not found",
                $"FileID needs the Windows App SDK 1.7 runtime to launch. " +
                $"Install it via:\n\n  winget install Microsoft.WindowsAppRuntime.1.7\n\n" +
                $"Error code: 0x{hr:X8}"
            );
            return hr;
        }
#endif

        // Reset each launch; lets an app that opens then closes be diagnosed without a debugger.
        var traceLogPath = System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
            "FileID", "logs", "startup-trace.txt");
        try
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(traceLogPath)!);
            System.IO.File.WriteAllText(traceLogPath, $"{System.DateTime.UtcNow:O} startup begin\n");
        }
        catch { /* trace is best-effort */ }

        void Trace(string msg)
        {
            try { System.IO.File.AppendAllText(traceLogPath, $"{System.DateTime.UtcNow:O} {msg}\n"); }
            catch { /* swallow */ }
        }

        try
        {
            Trace("ComWrappersSupport.InitializeComWrappers");
            ComWrappersSupport.InitializeComWrappers();
            Trace("Application.Start");
            Application.Start(parameters =>
            {
                try
                {
                    // Makes `await` resume on the UI thread by default.
                    Trace("SynchronizationContext bridge");
                    var ctx = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
                    SynchronizationContext.SetSynchronizationContext(ctx);
                    Trace("new App()");
                    _ = new App();
                    Trace("Application.Start callback returned");
                }
                catch (System.Exception inner)
                {
                    Trace($"FATAL inside Application.Start: {inner}");
                    throw;
                }
            });
            Trace("Application.Start returned (clean exit)");
        }
        catch (System.Exception ex)
        {
            Trace($"FATAL outside Application.Start: {ex}");
            ShowFatalDialog("FileID failed to start",
                $"FileID hit an unhandled exception during startup. The full trace is at:\n\n" +
                $"{traceLogPath}\n\n" +
                $"Error:\n{ex.GetType().Name}: {ex.Message}\n\n" +
                $"{ex.StackTrace}");
            return 1;
        }
        finally
        {
#if !FILEID_STORE
            Trace("Bootstrap.Shutdown");
            Bootstrap.Shutdown();
#endif
            Trace("end");
        }

        return 0;
    }

    private static void ShowFatalDialog(string title, string message)
    {
        _ = NativeMessageBox(System.IntPtr.Zero, message, title, 0x10u);
    }

    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "MessageBoxW", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
    private static extern int NativeMessageBox(System.IntPtr hWnd, string text, string caption, uint type);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc enumProc, System.IntPtr lParam);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(System.IntPtr hWnd, out uint lpdwProcessId);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool ShowWindow(System.IntPtr hWnd, int nCmdShow);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(System.IntPtr hWnd);
    private delegate bool EnumWindowsProc(System.IntPtr hWnd, System.IntPtr lParam);

    private static void FocusProcessWindow(int processId)
    {
        EnumWindows((hWnd, _) =>
        {
            uint threadId = GetWindowThreadProcessId(hWnd, out uint pid);
            if (threadId != 0 && pid == (uint)processId)
            {
                ShowWindow(hWnd, 9); // SW_RESTORE
                ShowWindow(hWnd, 5); // SW_SHOW
                SetForegroundWindow(hWnd);
            }
            return true;
        }, System.IntPtr.Zero);
    }
}
