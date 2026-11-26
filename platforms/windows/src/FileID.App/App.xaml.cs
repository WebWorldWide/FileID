using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;

namespace FileID;

public partial class App : Application
{
    public static Window? HostWindow { get; private set; }

    public static Task EngineStartedTask { get; private set; } = Task.CompletedTask;

    public App()
    {
        DebugSettings.XamlResourceReferenceFailed += (_, e) =>
            DebugLog.Warn("[XAML-RESOURCE] " + e.Message);
        InitializeComponent();
        UnhandledException += OnUnhandledException;
        // Thread-pool exceptions bypass WinUI's UnhandledException and surface only through AppDomain.
        System.AppDomain.CurrentDomain.UnhandledException += OnAppDomainUnhandled;
        // Fires only when a faulted task is finalized; identifies missing awaits.
        System.Threading.Tasks.TaskScheduler.UnobservedTaskException += OnUnobservedTaskException;
    }

    private void EnsureApplicationResources()
    {
        try
        {
            if (Resources.MergedDictionaries.Count == 0)
            {
                Resources.MergedDictionaries.Add(new Microsoft.UI.Xaml.Controls.XamlControlsResources());
                Resources.MergedDictionaries.Add(new ResourceDictionary
                {
                    Source = new System.Uri("ms-appx:///AppResources/Theme.xaml"),
                });
                DebugLog.Warn("[XAML-RESOURCE] Rebuilt missing application resource dictionaries at startup.");
            }

            if (!Resources.ContainsKey("BoolToVisibility"))
                Resources["BoolToVisibility"] = new FileID.Converters.BoolToVisibilityConverter();
            if (!Resources.ContainsKey("NullToVisibility"))
                Resources["NullToVisibility"] = new FileID.Converters.NullToVisibilityConverter();
            if (!Resources.ContainsKey("IdentityDouble"))
                Resources["IdentityDouble"] = new FileID.Converters.IdentityDoubleConverter();

            var hasGoldBrush = Resources.ContainsKey("GoldBrush") ||
                Resources.MergedDictionaries.Any(dictionary => dictionary.ContainsKey("GoldBrush"));
            if (!hasGoldBrush)
            {
                DebugLog.Warn("[XAML-RESOURCE] GoldBrush is still unavailable after restoring application dictionaries.");
            }
        }
        catch (System.Exception ex)
        {
            DebugLog.Error("[XAML-RESOURCE] Application resource recovery failed: " + ex);
            throw;
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Every startup step is wrapped so a failure shows a Win32 MessageBox instead of silently closing the window.
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
            EnsureApplicationResources();
            Trace("InspectXamlResources");
            try
            {
                var resources = Resources;
                Trace($"XAML app resources: keys={resources.Count}, merged={resources.MergedDictionaries.Count}");
                for (var i = 0; i < resources.MergedDictionaries.Count; i++)
                {
                    var dictionary = resources.MergedDictionaries[i];
                    var hasGoldBrush = dictionary.ContainsKey("GoldBrush");
                    Trace($"XAML merged[{i}]: source={dictionary.Source}, keys={dictionary.Count}, GoldBrush={hasGoldBrush}");
                }
            }
            catch (System.Exception ex)
            {
                Trace($"XAML resource audit failed (continuing): {ex.GetType().Name}: {ex.Message}");
            }
            // Off the UI thread: priming walks tens of thousands of cache files and would stall first paint.
            // Writes landing before Prime finishes are race-safe (the index is concurrent, the byte counter Interlocked).
            _ = Task.Run(ThumbnailDiskCache.Prime);
            // Detects a previous session that died by native fast-fail, which bypasses every managed crash sink.
            DebugLog.BeginSession();
            var asmVersion = typeof(App).Assembly.GetName().Version?.ToString() ?? "unknown";
            var enginePath = AppPaths.EngineExePath;
            DebugLog.Info($"[STARTUP] FileID app launched. version={asmVersion}, pid={Environment.ProcessId}, stateDir={PathRedactor.Redact(AppPaths.Root)}, engine={PathRedactor.Redact(enginePath)}");

            Trace("EngineClient.StartAsync");
            // Kept on EngineStartedTask so the install flow can await readiness.
            EngineStartedTask = EngineClient.Instance.StartAsync();
            _ = EngineStartedTask.ContinueWith(t =>
            {
                if (t.IsFaulted)
                {
                    Trace($"EngineClient.StartAsync faulted: {t.Exception?.GetBaseException()}");
                    DebugLog.Error("EngineClient.StartAsync faulted: " + t.Exception?.GetBaseException());
                }
                // Unrecoverable startup crashes (binary missing, untrusted signature, unsigned binary in a
                // release build) get a MessageBox; recoverable ones use the sidebar engine pill alone.
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

            Trace("WorkflowAutoTabRouter.Hook");
            try { WorkflowAutoTabRouter.Hook(); }
            catch (System.Exception ex) { Trace($"WorkflowAutoTabRouter.Hook failed (non-fatal): {ex.Message}"); }

            Trace("new MainWindow()");
            var window = new MainWindow();
            HostWindow = window;
            Trace("window.Activate()");
            window.Activate();
            try
            {
                var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
                _ = ShowWindow(hwnd, 9 /* SW_RESTORE */);
                _ = ShowWindow(hwnd, 5 /* SW_SHOW */);
                _ = SetForegroundWindow(hwnd);
                window.AppWindow.Show(true);
            }
            catch (System.Exception ex) { Trace($"Window show failed: {ex.Message}"); }
            Trace("OnLaunched complete");

            // Test-harness hook (--auto-scan-folder). The [AUTO-SCAN] log lines let the harness assert it ran.
            if (!string.IsNullOrEmpty(Program.AutoScanFolder))
            {
                _ = AutoScanAsync(Program.AutoScanFolder!, Program.AutoExitAfterScan, window);
            }
        }
        catch (System.Exception ex)
        {
            // Last resort: trace, log, and show a dialog so the user sees why the window closed.
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

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(System.IntPtr hWnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool ShowWindow(System.IntPtr hWnd, int nCmdShow);

    /// <summary>Test-harness scan: waits for the engine, scans the folder, and with --auto-exit-after-scan closes the window afterwards so the clean-exit marker is written.</summary>
    private static async Task AutoScanAsync(string folderPath, bool exitAfterScan, Window window)
    {
        var engine = EngineClient.Instance;
        try
        {
            DebugLog.Info($"[AUTO-SCAN] requested path={PathRedactor.Redact(folderPath)} exitAfter={exitAfterScan}");
            await engine.WaitForReadyAsync(TimeSpan.FromSeconds(60));
            if (engine.State != ViewModels.EngineClient.LifecycleState.Ready)
            {
                DebugLog.Error($"[AUTO-SCAN] engine not Ready after 60s (state={engine.State}); aborting.");
                if (exitAfterScan) window.DispatcherQueue.TryEnqueue(() => window.Close());
                return;
            }

            AppViewModel.Instance.FolderPath = folderPath;
            DebugLog.Info($"[AUTO-SCAN] starting scan; display={PathRedactor.Redact(AppViewModel.Instance.FolderDisplay)}");
            if (!exitAfterScan)
            {
                await engine.StartScanAsync(folderPath, AppViewModel.Instance.FolderDisplay);
                return;
            }

            var scanGeneration = engine.SpawnGeneration;
            var scanTerminal = new TaskCompletionSource<FileID.IpcSchema.ScanPhase>(
                TaskCreationOptions.RunContinuationsAsynchronously);
            TaskCompletionSource<FileID.IpcSchema.FaceClusteringResult>? faceTerminal =
                new(TaskCreationOptions.RunContinuationsAsynchronously);
            var applyDrained = new TaskCompletionSource<bool>(
                TaskCreationOptions.RunContinuationsAsynchronously);

            void OnEngineChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
                => DebugLog.SafeRun(nameof(OnEngineChanged), () =>
                {
                    if (engine.SpawnGeneration != scanGeneration)
                    {
                        scanTerminal.TrySetException(new InvalidOperationException(
                            "Engine generation changed before the auto-scan finished."));
                        return;
                    }

                    if (e.PropertyName == nameof(ViewModels.EngineClient.State)
                        && engine.State == ViewModels.EngineClient.LifecycleState.Crashed)
                    {
                        scanTerminal.TrySetException(new InvalidOperationException(
                            engine.CrashReason ?? "The engine stopped during the auto-scan."));
                        return;
                    }

                    if (e.PropertyName == nameof(ViewModels.EngineClient.Phase)
                        && engine.Phase is FileID.IpcSchema.ScanPhase.Completed
                            or FileID.IpcSchema.ScanPhase.Failed
                            or FileID.IpcSchema.ScanPhase.Cancelled)
                    {
                        scanTerminal.TrySetResult(engine.Phase.Value);
                        window.DispatcherQueue.TryEnqueue(() => applyDrained.TrySetResult(true));
                    }

                    if (e.PropertyName == nameof(ViewModels.EngineClient.LastFaceClustering)
                        && engine.LastFaceClustering is { } faceResult)
                    {
                        faceTerminal.TrySetResult(faceResult);
                    }
                });

            engine.PropertyChanged += OnEngineChanged;
            try
            {
                await engine.StartScanAsync(folderPath, AppViewModel.Instance.FolderDisplay);
                var terminalPhase = await scanTerminal.Task.WaitAsync(TimeSpan.FromHours(12));
                await applyDrained.Task.WaitAsync(TimeSpan.FromSeconds(30));
                FileID.IpcSchema.FaceClusteringResult? faceResult = null;
                if (terminalPhase == FileID.IpcSchema.ScanPhase.Completed)
                {
                    faceResult = await faceTerminal!.Task.WaitAsync(TimeSpan.FromHours(12));
                    DebugLog.Info("[AUTO-SCAN] face clustering completed");
                }

                DebugLog.Info(
                    $"[AUTO-SCAN] scan ended phase={terminalPhase} processed={EngineClient.Instance.LastScanProcessedFiles} persons={faceResult?.PersonCount ?? 0}");
            }
            finally
            {
                engine.PropertyChanged -= OnEngineChanged;
            }

            if (exitAfterScan)
            {
                window.DispatcherQueue.TryEnqueue(() =>
                {
                    try { window.Close(); }
                    catch { }
                });
            }
        }
        catch (Exception ex)
        {
            DebugLog.Error($"[AUTO-SCAN] {ex.GetType().Name}: {ex.Message}");
            if (exitAfterScan)
            {
                window.DispatcherQueue.TryEnqueue(() =>
                {
                    try { window.Close(); }
                    catch { }
                });
            }
        }
    }
    private void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        // Dump first, then recover.
        var path = DebugLog.WriteCrashDump("WinUI Application.UnhandledException", e.Exception, terminating: false);
        DebugLog.Error("Unhandled: " + e.Exception + (string.IsNullOrEmpty(path) ? "" : $" (dump: {path})"));
        // Recover rather than terminate: the engine holds scan progress that a kill would lose.
        e.Handled = true;
    }

    private static void OnAppDomainUnhandled(object sender, System.UnhandledExceptionEventArgs e)
    {
        // The CLR is unwinding; only log for the next session.
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
