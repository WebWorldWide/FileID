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
        // Background-thread exceptions (Task.Run continuations, ConfigureAwait(false)
        // resumes that throw on a thread-pool thread) bypass WinUI's
        // UnhandledException — they bubble through AppDomain. Log to disk
        // so a tab-swap-mid-scan crash leaves a forensic trail next session.
        System.AppDomain.CurrentDomain.UnhandledException += OnAppDomainUnhandled;
        // Tasks whose result/exception is never observed surface here only
        // when the finalizer runs. Still worth logging — it identifies
        // missing awaits / fire-and-forget bugs.
        System.Threading.Tasks.TaskScheduler.UnobservedTaskException += OnUnobservedTaskException;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // defensive startup. Every step is wrapped so a single
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
            // Prime the disk thumbnail cache size counter so Settings →
            // Diagnostics shows a real number without waiting for the
            // first sweep. Cheap one-shot directory walk.
            ThumbnailDiskCache.Prime();
            // last-session breadcrumb. Detects whether the
            // previous session died via a native fast-fail (which
            // bypasses every managed crash sink) and writes a
            // forensic artifact if so. Always-run; idempotent.
            DebugLog.BeginSession();
            // structured startup line so a `crash-*.txt` from any
            // session can be correlated to a specific build + engine binary.
            var asmVersion = typeof(App).Assembly.GetName().Version?.ToString() ?? "unknown";
            var enginePath = AppPaths.EngineExePath;
            DebugLog.Info($"[STARTUP] FileID app launched. version={asmVersion}, pid={Environment.ProcessId}, stateDir={AppPaths.Root}, engine={enginePath}");

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
                // Surface unrecoverable startup crashes visibly. The sidebar
                // engine pill (round-2 auto-hide) shows them too, but new
                // users won't know where to look — a Win32 MessageBox is
                // hard to miss. Covers three classes:
                //   1. Binary not found (path resolution failed)
                //   2. Signature verdict was Untrusted (tamper / bad cert chain)
                //   3. Release build refused an Unsigned binary (FILEID_EV_THUMBPRINT set)
                // Recoverable crashes (pure spawn failures, runtime panics
                // that respawn) keep using the pill alone.
                try
                {
                    if (EngineClient.Instance.State == ViewModels.EngineClient.LifecycleState.Crashed)
                    {
                        var reason = EngineClient.Instance.CrashReason ?? string.Empty;
                        string? title = null;
                        string? body = null;
                        if (reason.Contains("not found", StringComparison.OrdinalIgnoreCase))
                        {
                            title = "FileID — engine missing";
                            body = "FileID couldn't find its engine binary.\n\n" +
                                   $"Expected at:\n{AppPaths.EngineExePath}\n\n" +
                                   "If you built from source, run the engine build script (build/build.ps1). " +
                                   "If you installed FileID via MSI, the install is incomplete — reinstall from your downloaded MSI.\n\n" +
                                   "See %LOCALAPPDATA%\\FileID\\logs\\app.log for details.";
                        }
                        else if (reason.Contains("signature verification failed", StringComparison.OrdinalIgnoreCase)
                                 || reason.Contains("Untrusted", StringComparison.OrdinalIgnoreCase)
                                 || reason.Contains("changed between Verify and spawn", StringComparison.OrdinalIgnoreCase))
                        {
                            title = "FileID — engine signature failed";
                            body = "FileID's engine binary failed Authenticode verification.\n\n" +
                                   "Reason: " + reason + "\n\n" +
                                   "This usually means the install is corrupt or tampered. " +
                                   "Reinstall FileID from your trusted source.\n\n" +
                                   "See %LOCALAPPDATA%\\FileID\\logs\\app.log for details.";
                        }
                        else if (reason.Contains("unsigned", StringComparison.OrdinalIgnoreCase))
                        {
                            title = "FileID — engine unsigned";
                            body = "FileID's engine binary is unsigned, but this release was built " +
                                   "to require a valid signature.\n\n" +
                                   "Reinstall FileID from a trusted source. If you built from source, " +
                                   "unset the FILEID_EV_THUMBPRINT environment variable for dev builds.\n\n" +
                                   "See %LOCALAPPDATA%\\FileID\\logs\\app.log for details.";
                        }
                        if (title is not null && body is not null)
                        {
                            _ = NativeMessageBox(System.IntPtr.Zero, body, title, 0x10u /* MB_ICONERROR */);
                        }
                    }
                }
                catch (System.Exception ex) { Trace($"engine-crash dialog failed: {ex.Message}"); }
            });

            Trace("ScanCompleteToast.Start");
            try { ScanCompleteToast.Start(); }
            catch (System.Exception ex) { Trace($"ScanCompleteToast.Start failed (non-fatal): {ex.Message}"); }

            Trace("CudaAutoInstaller.Hook");
            try { CudaAutoInstaller.Hook(); }
            catch (System.Exception ex) { Trace($"CudaAutoInstaller.Hook failed (non-fatal): {ex.Message}"); }

            Trace("LlamaRuntimeAutoInstaller.Hook");
            try { LlamaRuntimeAutoInstaller.Hook(); }
            catch (System.Exception ex) { Trace($"LlamaRuntimeAutoInstaller.Hook failed (non-fatal): {ex.Message}"); }

            // CudnnAutoInstaller silent fetch removed. The ~430 MB
            // background download surprised users and added startup-time
            // GPU pressure during what was already a hang-prone period.
            // DirectML at 38 fps is sufficient on the RTX 2060 / 6 GB
            // target. Power users can opt in via Settings → Performance
            // → "NVIDIA acceleration (cuDNN)". See DECISIONS.md 2026-05-15.

            Trace("WorkflowAutoTabRouter.Hook");
            try { WorkflowAutoTabRouter.Hook(); }
            catch (System.Exception ex) { Trace($"WorkflowAutoTabRouter.Hook failed (non-fatal): {ex.Message}"); }

            Trace("new MainWindow()");
            var window = new MainWindow();
            HostWindow = window;
            Trace("window.Activate()");
            window.Activate();
            Trace("OnLaunched complete");

            // harness auto-scan. When Program.AutoScanFolder is set
            // (--auto-scan-folder CLI arg), kick off a scan once the engine
            // reaches Ready. Fire-and-forget on purpose; failures land in
            // app.log via the catch. The `[AUTO-SCAN]` marker lets the
            // harness grep the log to assert the path actually ran.
            if (!string.IsNullOrEmpty(Program.AutoScanFolder))
            {
                _ = AutoScanAsync(Program.AutoScanFolder!, Program.AutoExitAfterScan, window);
            }
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

    /// <summary>harness entry point. Awaits engine readiness, sets
    /// the folder on AppViewModel (so the UI reflects it), kicks off
    /// StartScanAsync, and — if --auto-exit-after-scan was passed — closes
    /// the window once ScanCompleteEvent fires so MarkCleanExit runs and
    /// the harness can assert clean_exit=true.</summary>
    private static async Task AutoScanAsync(string folderPath, bool exitAfterScan, Window window)
    {
        try
        {
            DebugLog.Info($"[AUTO-SCAN] requested path={folderPath} exitAfter={exitAfterScan}");
            await EngineClient.Instance.WaitForReadyAsync(System.TimeSpan.FromSeconds(60));
            if (EngineClient.Instance.State != ViewModels.EngineClient.LifecycleState.Ready)
            {
                DebugLog.Error($"[AUTO-SCAN] engine not Ready after 60s (state={EngineClient.Instance.State}); aborting.");
                if (exitAfterScan) { window.DispatcherQueue.TryEnqueue(() => window.Close()); }
                return;
            }
            AppViewModel.Instance.FolderPath = folderPath;
            DebugLog.Info($"[AUTO-SCAN] starting scan; display={AppViewModel.Instance.FolderDisplay}");
            await EngineClient.Instance.StartScanAsync(folderPath, AppViewModel.Instance.FolderDisplay);
            if (!exitAfterScan) return;

            // Wait for ScanComplete by watching Phase transition to Completed
            // (or Failed). PropertyChanged fires on whatever thread the
            // engine event arrived on — don't touch XAML in the handler.
            var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            void OnEngineChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
            {
                if (e.PropertyName != nameof(ViewModels.EngineClient.Phase)) return;
                var phase = EngineClient.Instance.Phase;
                if (phase == FileID.IpcSchema.ScanPhase.Completed || phase == FileID.IpcSchema.ScanPhase.Failed)
                {
                    tcs.TrySetResult(phase == FileID.IpcSchema.ScanPhase.Completed);
                }
            }
            EngineClient.Instance.PropertyChanged += OnEngineChanged;
            try
            {
                var ok = await tcs.Task;
                DebugLog.Info($"[AUTO-SCAN] scan ended ok={ok}; closing window.");
            }
            finally
            {
                EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            }
            window.DispatcherQueue.TryEnqueue(() => { try { window.Close(); } catch { } });
        }
        catch (System.Exception ex)
        {
            DebugLog.Error($"[AUTO-SCAN] failed: {ex.GetType().Name}: {ex.Message}");
            if (exitAfterScan) { window.DispatcherQueue.TryEnqueue(() => { try { window.Close(); } catch { } }); }
        }
    }

    /// <summary>
    /// Last-resort handler for exceptions that escape the dispatcher loop.
    /// Logs locally; does NOT phone home (privacy guarantee).
    ///
    /// PRIVACY: this method must never make a network call. Reviewed every
    /// PR that touches it.
    /// </summary>
    private void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        // write a dedicated crash dump BEFORE deciding whether to
        // recover. Captures last 50 lines of app.log for context — until
        // this round we had no forensic data on UI-thread crashes.
        var path = DebugLog.WriteCrashDump("WinUI Application.UnhandledException", e.Exception, terminating: false);
        DebugLog.Error("Unhandled: " + e.Exception + (string.IsNullOrEmpty(path) ? "" : $" (dump: {path})"));
        // Recover rather than terminate. The user has unsaved scan state in
        // the engine; killing the process loses progress. Unrecoverable
        // exceptions (StackOverflow, OOM) bypass this handler anyway. WER
        // crash dumps for diagnosis trade off against the user's open
        // session — for an on-device app the user's session wins.
        e.Handled = true;
    }

    private static void OnAppDomainUnhandled(object sender, System.UnhandledExceptionEventArgs e)
    {
        // Cannot recover here (terminating == true means CLR is unwinding).
        // Log to disk so the next session has forensic info; the user reported
        // "clicking sidebar during a scan crashes the entire app" and these
        // background-thread crashes are the most likely culprit.
        try
        {
            var ex = e.ExceptionObject as Exception;
            var path = DebugLog.WriteCrashDump("AppDomain.UnhandledException", ex, terminating: e.IsTerminating);
            DebugLog.Error("AppDomain.Unhandled (terminating=" + e.IsTerminating + "): " + e.ExceptionObject + (string.IsNullOrEmpty(path) ? "" : $" (dump: {path})"));
        }
        catch { }
    }

    private static void OnUnobservedTaskException(object? sender, System.Threading.Tasks.UnobservedTaskExceptionEventArgs e)
    {
        try
        {
            var path = DebugLog.WriteCrashDump("TaskScheduler.UnobservedTaskException", e.Exception, terminating: false);
            DebugLog.Error("UnobservedTaskException: " + e.Exception + (string.IsNullOrEmpty(path) ? "" : $" (dump: {path})"));
        }
        catch { }
        e.SetObserved();
    }
}
