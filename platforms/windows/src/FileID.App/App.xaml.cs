// Application root — owns the single MainWindow and the lifetime of every
// app-level service (EngineClient, model installers, settings store).
//
// On macOS this is FileIDApp.swift + AppDelegate. On Windows the WinUI
// `Application` plays both roles: lifecycle hooks land in OnLaunched here.

using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;

namespace FileID;

public partial class App : Application
{
    /// <summary>
    /// The single host Window. Code that needs an HWND (folder picker,
    /// dialogs, drag-region anchors) reaches in here.
    /// </summary>
    public static Window? HostWindow { get; private set; }

    /// <summary>
    /// Awaitable handle on the engine-spawn task. The Welcome sheet's
    /// Install handlers no longer need this directly (they go through
    /// EngineClient.WaitForReadyAsync), but exposing it lets any other
    /// startup-sensitive code observe whether the spawn faulted.
    /// </summary>
    public static Task EngineStartedTask { get; private set; } = Task.CompletedTask;

    public App()
    {
        InitializeComponent();
        UnhandledException += OnUnhandledException;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // V14.7.2: defensive startup. Every step is wrapped so a single
        // failure surfaces a Win32 MessageBox instead of silently
        // closing the window. The fail-visibly path is what makes
        // "opens then instantly closes" diagnosable.
        var traceLogPath = System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
            "FileID", "logs", "startup-trace.txt");
        void Trace(string msg)
        {
            try { System.IO.File.AppendAllText(traceLogPath, $"{System.DateTime.UtcNow:O} OnLaunched: {msg}\n"); }
            catch { }
        }
        try
        {
            Trace("EnsureDirectories");
            AppPaths.EnsureDirectories();
            Trace($"State dir = {AppPaths.Root}");
            DebugLog.Info($"FileID launched. State dir: {AppPaths.Root}");

            Trace("EngineClient.StartAsync");
            // Captured on EngineStartedTask so the install flow can poll
            // / await readiness via EngineClient.WaitForReadyAsync. The
            // continuation still logs faults locally (no telemetry).
            EngineStartedTask = EngineClient.Instance.StartAsync();
            _ = EngineStartedTask.ContinueWith(t =>
            {
                if (t.IsFaulted)
                {
                    Trace($"EngineClient.StartAsync faulted: {t.Exception?.GetBaseException()}");
                    DebugLog.Error("EngineClient.StartAsync faulted: " + t.Exception?.GetBaseException());
                }
            });

            Trace("ScanCompleteToast.Start");
            try { ScanCompleteToast.Start(); }
            catch (System.Exception ex) { Trace($"ScanCompleteToast.Start failed (non-fatal): {ex.Message}"); }

            Trace("new MainWindow()");
            var window = new MainWindow();
            HostWindow = window;
            Trace("window.Activate()");
            window.Activate();
            Trace("OnLaunched complete");
        }
        catch (System.Exception ex)
        {
            // Last-resort: write to trace, log, and surface a Win32
            // dialog so the user sees WHY the window closed.
            Trace($"FATAL: {ex.GetType().Name}: {ex.Message}\n{ex.StackTrace}");
            try { DebugLog.Error("OnLaunched FATAL: " + ex); } catch { }
            try
            {
                _ = NativeMessageBox(
                    System.IntPtr.Zero,
                    $"FileID hit an error during startup:\n\n{ex.GetType().Name}: {ex.Message}\n\nFull trace:\n{traceLogPath}",
                    "FileID — startup error",
                    0x10u);
            }
            catch { }
            throw;
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "MessageBoxW", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
    private static extern int NativeMessageBox(System.IntPtr hWnd, string text, string caption, uint type);

    /// <summary>
    /// Last-resort handler for exceptions that escape the dispatcher loop.
    /// Logs locally; does NOT phone home (privacy guarantee).
    ///
    /// PRIVACY: this method must never make a network call. Reviewed every
    /// PR that touches it.
    /// </summary>
    private void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        DebugLog.Error("Unhandled: " + e.Exception);
        // Set Handled=false so WER produces a local crash dump (matches
        // macOS letting CrashReporter take over). The user can attach
        // %LOCALAPPDATA%\FileID\logs\app.log alongside.
        e.Handled = false;
    }
}
