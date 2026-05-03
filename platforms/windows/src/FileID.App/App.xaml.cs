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

    public App()
    {
        InitializeComponent();
        UnhandledException += OnUnhandledException;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        AppPaths.EnsureDirectories();
        DebugLog.Info($"FileID launched. State dir: {AppPaths.Root}");

        // Start the engine first thing — UI binds reactively, so the
        // engine spawn racing the first frame is fine. EngineClient handles
        // not-yet-ready by exposing State = Starting.
        _ = EngineClient.Instance.StartAsync();

        // Subscribe the toast service before any scan completes.
        ScanCompleteToast.Start();

        var window = new MainWindow();
        HostWindow = window;
        window.Activate();
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
        DebugLog.Error("Unhandled: " + e.Exception);
        // Set Handled=false so WER produces a local crash dump (matches
        // macOS letting CrashReporter take over). The user can attach
        // %LOCALAPPDATA%\FileID\logs\app.log alongside.
        e.Handled = false;
    }
}
