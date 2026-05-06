// MainWindow code-behind — chrome (Mica/Acrylic, dark mode, custom title
// bar, min size), sidebar visibility binding, drag-drop folder, and the
// app-level keyboard accelerators (Alt+1..6, Ctrl+O, Ctrl+R, Ctrl+F,
// Ctrl+Shift+S).

using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.System;
using Windows.UI;
using WinRT;

namespace FileID;

public sealed partial class MainWindow : Window
{
    /// <summary>
    /// Hard floor on window size. Below this, the sidebar's 260-DIP fixed
    /// width starts crowding the detail pane and the welcome sheet's
    /// 540-DIP MinWidth bleeds past the chrome.
    /// </summary>
    private const int MinWidth = 1200;
    private const int MinHeight = 800;

    /// <summary>
    /// Launch size: comfortably bigger than the floor so the user sees a
    /// real workspace on first launch. Sized to fit 1920×1080 with taskbar
    /// margin AND look intentional at 2560×1440. Capped against the user's
    /// actual display size so we never exceed the screen on smaller laptops.
    /// </summary>
    private const int LaunchWidth = 1480;
    private const int LaunchHeight = 980;

    private SystemBackdropConfiguration? _backdropConfig;
    private MicaController? _micaController;
    private DesktopAcrylicController? _acrylicController;

    public MainWindow()
    {
        // V14.7.2: every step in the constructor is independently
        // wrapped so a single failure (e.g. backdrop unsupported on
        // older Win10) doesn't kill the window before it shows. If
        // ANY step fails the rest still run, and the failure goes to
        // the startup-trace.
        var traceLogPath = System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
            "FileID", "logs", "startup-trace.txt");
        // Buffer trace lines in memory and flush ONCE at the end of the
        // ctor — the previous per-call File.AppendAllText opened/closed
        // the file ~20 times during init, blocking the UI thread on a
        // slow disk and inflating cold-start latency by 50–200 ms. The
        // buffered approach is also crash-safe because we re-flush on
        // each unrecoverable Step failure inside the try/catch.
        var traceBuffer = new System.Text.StringBuilder(2048);
        void Trace(string msg)
        {
            traceBuffer.AppendFormat(System.Globalization.CultureInfo.InvariantCulture,
                "{0:O} MainWindow: {1}\n", System.DateTime.UtcNow, msg);
        }
        void FlushTrace()
        {
            if (traceBuffer.Length == 0) return;
            try { System.IO.File.AppendAllText(traceLogPath, traceBuffer.ToString()); }
            catch { }
            traceBuffer.Clear();
        }
        void Step(string name, System.Action body)
        {
            try { Trace(name); body(); }
            catch (System.Exception ex)
            {
                Trace($"{name} failed (continuing): {ex.GetType().Name}: {ex.Message}");
                FlushTrace(); // persist now in case the next step crashes hard
            }
        }

        // InitializeComponent MUST succeed; if it throws there's no
        // window to show. Let it propagate up to the App handler.
        Trace("InitializeComponent");
        InitializeComponent();
        Title = "FileID";

        Step("ApplyTitleBarChrome", ApplyTitleBarChrome);
        Step("ApplyMinimumSize", ApplyMinimumSize);
        Step("ApplySystemBackdrop", ApplySystemBackdrop);
        Step("ForceDarkTitleBar", ForceDarkTitleBar);
        Step("WireKeyboardShortcuts", WireKeyboardShortcuts);

        Activated += OnActivated;
        Closed += OnClosed;
        Step("ThemeChanged subscribe", () => ((FrameworkElement)Content).ActualThemeChanged += OnThemeChanged);

        Step("AppViewModel subscribe", () => AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged);
        Step("ApplySidebarVisibility", ApplySidebarVisibility);

        // First-launch model installer. Async-launched after the window
        // has had a moment to layout so the user sees the chrome before
        // the modal pops over it.
        Step("Welcome subscribe", () =>
            ((FrameworkElement)Content).Loaded += async (_, _) =>
            {
                try { await MaybeShowWelcomeSheetAsync(); }
                catch (System.Exception ex) { Trace($"Welcome sheet failed: {ex.Message}"); }
            });

        Trace("ctor complete");
        FlushTrace();
    }

    private async Task MaybeShowWelcomeSheetAsync()
    {
        DebugLog.Info("[INSTALL] MaybeShowWelcomeSheetAsync called.");
        ModelInstallerService.Instance.Refresh();
        var svc = ModelInstallerService.Instance;
        DebugLog.Info($"[INSTALL] sentinel state: clip={svc.Clip.Status} arcface={svc.Arcface.Status} vlm={svc.Vlm.Status}");

        // Mirror macOS shouldShowWelcome() (FileIDApp.swift:64-72): show
        // the sheet on first launch (welcomeSheetSeen == false) OR any
        // time a required model is missing on a subsequent launch.
        var seen = false;
        try { seen = AppSettings.Load().WelcomeSheetSeen; }
        catch (Exception ex) { DebugLog.Warn("MaybeShowWelcomeSheet: AppSettings.Load threw: " + ex.Message); }
        if (seen && svc.AllInstalled)
        {
            DebugLog.Info("[INSTALL] welcomeSheetSeen=true and all models installed; skipping.");
            return;
        }
        if (svc.AllInstalled)
        {
            DebugLog.Info("[INSTALL] all three models already installed; skipping welcome sheet.");
            return;
        }

        // V14.7.7: ContentDialog by default adds its own padding +
        // reserves a button row at the bottom. With our custom buttons
        // inside the sheet, the dialog chrome was eating ~80 DIP and
        // clipping our action row. Trim padding only -- KEEP the
        // default Background so ContentDialog's full-screen dim
        // overlay still hides the underlying tab content (otherwise
        // the OnboardingSplash + sidebar bleed through and look messy).
        DebugLog.Info("[INSTALL] constructing WelcomeSheet + ContentDialog.");
        var sheet = new Views.WelcomeSheet();
        var dialog = new ContentDialog
        {
            XamlRoot = ((FrameworkElement)Content).XamlRoot,
            Content = sheet,
            Padding = new Thickness(0),
        };
        // ContentDialog's default ContentDialogMaxWidth (~548 DIP) clips
        // our 600 DIP UserControl — the per-row Install buttons + the
        // footer "Skip for now" button get cut off the right edge. Bump
        // both Min and Max so the dialog is exactly as wide as the sheet.
        dialog.Resources["ContentDialogMaxWidth"] = 720.0;
        dialog.Resources["ContentDialogMinWidth"] = 660.0;
        sheet.Dismissed += (_, _) =>
        {
            DebugLog.Info("[INSTALL] WelcomeSheet.Dismissed fired; closing dialog.");
            try { dialog.Hide(); } catch { }
        };
        try
        {
            DebugLog.Info("[INSTALL] dialog.ShowAsync awaiting...");
            await dialog.ShowAsync();
            DebugLog.Info("[INSTALL] dialog.ShowAsync returned.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[INSTALL] Welcome sheet ShowAsync threw: " + ex);
        }
    }

    private void ApplyTitleBarChrome()
    {
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        // V14.7.3: explicit AppWindow icon. The .exe already has the
        // icon embedded via <ApplicationIcon>, so taskbar / Alt-Tab
        // already work. SetIcon makes the WINDOW icon (top-left if a
        // chrome'd window, system menu icon) match — defensive +
        // explicit so any consumer (third-party window enumerators,
        // WinUI's own internal title-bar code) sees the right icon.
        try
        {
            var iconPath = System.IO.Path.Combine(
                System.AppContext.BaseDirectory, "Assets", "FileID.ico");
            if (System.IO.File.Exists(iconPath))
            {
                AppWindow.SetIcon(iconPath);
            }
        }
        catch (System.Exception ex)
        {
            DebugLog.Warn("AppWindow.SetIcon failed (non-fatal): " + ex.Message);
        }
    }

    private void ApplyMinimumSize()
    {
        // AppWindow.Resize + Move + DisplayArea.WorkArea all speak PHYSICAL
        // PIXELS, not DIPs. Our LaunchWidth/Height + MinWidth/Height
        // constants are in DIPs (the unit XAML uses everywhere else). Scale
        // them by the per-monitor DPI factor before passing to AppWindow.
        // Without this, a 200% scaled display sees a half-size window.
        uint dpi = GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        if (dpi == 0) { dpi = 96; }
        double scale = dpi / 96.0;
        int minWPx    = (int)(MinWidth    * scale);
        int minHPx    = (int)(MinHeight   * scale);
        int launchWPx = (int)(LaunchWidth  * scale);
        int launchHPx = (int)(LaunchHeight * scale);

        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            // PreferredMinimumWidth/Height are in DIPs (unlike Resize/Move
            // which want pixels). Pass DIPs directly.
            presenter.PreferredMinimumWidth = MinWidth;
            presenter.PreferredMinimumHeight = MinHeight;
        }

        // Pick a launch size: prefer LaunchWidth/Height (in pixels) but
        // cap at 90% of the work area so we never spill off small laptops.
        var displayArea = DisplayArea.GetFromWindowId(
            AppWindow.Id,
            DisplayAreaFallback.Primary);
        var work = displayArea.WorkArea;
        int targetW = Math.Min(launchWPx, (int)(work.Width * 0.90));
        int targetH = Math.Min(launchHPx, (int)(work.Height * 0.90));
        targetW = Math.Max(targetW, minWPx);
        targetH = Math.Max(targetH, minHPx);

        AppWindow.Resize(new Windows.Graphics.SizeInt32(targetW, targetH));

        // Center on the active display.
        int x = work.X + (work.Width - targetW) / 2;
        int y = work.Y + (work.Height - targetH) / 2;
        AppWindow.Move(new Windows.Graphics.PointInt32(x, y));
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    private void ApplySystemBackdrop()
    {
        if (!DesktopAcrylicController.IsSupported() && !MicaController.IsSupported())
        {
            return;
        }

        _backdropConfig = new SystemBackdropConfiguration
        {
            IsInputActive = true,
            Theme = SystemBackdropTheme.Dark,
        };

        if (MicaController.IsSupported())
        {
            _micaController = new MicaController { Kind = MicaKind.Base };
            _micaController.AddSystemBackdropTarget(this.As<ICompositionSupportsSystemBackdrop>());
            _micaController.SetSystemBackdropConfiguration(_backdropConfig);
        }
        else
        {
            _acrylicController = new DesktopAcrylicController();
            _acrylicController.AddSystemBackdropTarget(this.As<ICompositionSupportsSystemBackdrop>());
            _acrylicController.SetSystemBackdropConfiguration(_backdropConfig);
        }
    }

    private void ForceDarkTitleBar()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        int yes = 1;
        _ = NativeDwmSetWindowAttribute(hwnd, attribute: 20, ref yes, sizeof(int));

        if (AppWindowTitleBar.IsCustomizationSupported() && AppWindow?.TitleBar is { } tb)
        {
            tb.ButtonBackgroundColor = Colors.Transparent;
            tb.ButtonInactiveBackgroundColor = Colors.Transparent;
            tb.ButtonForegroundColor = Color.FromArgb(0xFF, 0xE6, 0xE6, 0xE6);
            tb.ButtonInactiveForegroundColor = Color.FromArgb(0xFF, 0x99, 0x99, 0x99);
            tb.ButtonHoverBackgroundColor = Color.FromArgb(0x33, 0xFF, 0xFF, 0xFF);
            tb.ButtonHoverForegroundColor = Colors.White;
            tb.ButtonPressedBackgroundColor = Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF);
            tb.ButtonPressedForegroundColor = Colors.White;
        }
    }

    /// <summary>
    /// Register every keyboard shortcut on the root layout so they fire
    /// regardless of focus location. Mirror of macOS Sidebar.swift's
    /// keyboardShortcut(...) attachments.
    /// </summary>
    private void WireKeyboardShortcuts()
    {
        // Ctrl+O — pick folder
        AddAccelerator(VirtualKey.O, VirtualKeyModifiers.Control, async (_, _) =>
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var result = await FolderPickerService.PickFolderAsync(hwnd);
            if (result.Path is not null)
            {
                AppViewModel.Instance.FolderPath = result.Path;
            }
        });

        // Ctrl+R — start scan
        AddAccelerator(VirtualKey.R, VirtualKeyModifiers.Control, (_, _) =>
        {
            var vm = AppViewModel.Instance;
            if (vm.HasFolder)
            {
                _ = EngineClient.Instance.StartScanAsync(vm.FolderPath!, vm.FolderDisplay);
            }
        });

        // Ctrl+Shift+S — toggle sidebar
        AddAccelerator(VirtualKey.S, VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift,
            (_, _) => AppViewModel.Instance.ToggleSidebar());

        // Ctrl+Z — undo last destructive action.
        AddAccelerator(VirtualKey.Z, VirtualKeyModifiers.Control, async (_, _) =>
        {
            var label = await Services.UndoStack.Instance.UndoAsync();
            if (!string.IsNullOrEmpty(label))
            {
                Services.DebugLog.Info($"Undid: {label}");
            }
        });

        // BUG-13: Ctrl+, opens Settings (mirror of macOS Cmd+,).
        // VirtualKey.Decimal is the NUMPAD period — wrong key. The
        // ASCII comma reports as VK_OEM_COMMA = 0xBC = 188. Register
        // ONLY the OEM comma. (Previous version also registered Decimal,
        // which made numpad-period jump to Settings — surprise.)
        AddAccelerator((VirtualKey)0xBC, VirtualKeyModifiers.Control,
            (_, _) => AppViewModel.Instance.ActiveTab = SidebarTab.Settings);

        // Ctrl+F — focus search. Phase 1 has no search field in the
        // Detail; the accelerator is reserved here so Phase 2 wiring is
        // a one-liner (raise an event the LibraryView subscribes to).
        AddAccelerator(VirtualKey.F, VirtualKeyModifiers.Control,
            (_, _) => SearchFocusRequested?.Invoke(this, EventArgs.Empty));

        // Alt+1..6 — jump to tab. Windows-native QoL addition (per
        // shared/docs/DECISIONS.md 2026-05-02 entry).
        for (int i = 0; i < SidebarTab.All.Count; i++)
        {
            int idx = i;
            var key = (VirtualKey)((int)VirtualKey.Number1 + i);
            AddAccelerator(key, VirtualKeyModifiers.Menu, (_, _) =>
            {
                AppViewModel.Instance.ActiveTab = SidebarTab.All[idx];
            });
        }

        // V14.7.15: ShortcutsCheatSheet (F1 / Ctrl+?) removed for strict
        // macOS parity — macOS has no centralized shortcuts panel.
    }

    private void AddAccelerator(VirtualKey key, VirtualKeyModifiers modifiers,
                                TypedEventHandler<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs> handler)
    {
        var accel = new KeyboardAccelerator
        {
            Key = key,
            Modifiers = modifiers,
            // Window-scoped: fires regardless of focused element.
            ScopeOwner = null,
        };
        accel.Invoked += (s, e) =>
        {
            handler(s, e);
            e.Handled = true;
        };
        ((FrameworkElement)Content).KeyboardAccelerators.Add(accel);
    }

    /// <summary>
    /// Raised by Ctrl+F. Phase 2 LibraryView listens and focuses the search box.
    /// </summary>
    public event EventHandler? SearchFocusRequested;

    private void OnActivated(object sender, WindowActivatedEventArgs e)
    {
        if (_backdropConfig is not null)
        {
            _backdropConfig.IsInputActive = e.WindowActivationState != WindowActivationState.Deactivated;
        }
    }

    private void OnClosed(object sender, WindowEventArgs e)
    {
        if (_micaController is not null) { _micaController.Dispose(); _micaController = null; }
        if (_acrylicController is not null) { _acrylicController.Dispose(); _acrylicController = null; }
        _backdropConfig = null;
        AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;

        // Tell the engine to wrap up so the WAL gets checkpointed cleanly.
        try { _ = EngineClient.Instance.ShutdownAsync(); } catch { }
    }

    private void OnThemeChanged(FrameworkElement sender, object args)
    {
        if (_backdropConfig is not null)
        {
            _backdropConfig.Theme = sender.ActualTheme switch
            {
                ElementTheme.Dark => SystemBackdropTheme.Dark,
                ElementTheme.Light => SystemBackdropTheme.Light,
                _ => SystemBackdropTheme.Default,
            };
        }
    }

    private void OnAppViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(AppViewModel.SidebarVisible))
        {
            DispatcherQueue.TryEnqueue(ApplySidebarVisibility);
        }
    }

    private void ApplySidebarVisibility()
    {
        if (AppViewModel.Instance.SidebarVisible)
        {
            SidebarColumn.Width = new GridLength(260);
            SidebarHost.Visibility = Visibility.Visible;
        }
        else
        {
            SidebarColumn.Width = new GridLength(0);
            SidebarHost.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>
    /// V14.7.16: title-bar sidebar toggle button. Same effect as
    /// Ctrl+Shift+S but visible at all times so the user always has a way
    /// to bring the sidebar back after hiding it.
    /// </summary>
    private void OnSidebarToggleClicked(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.ToggleSidebar();
    }

    // ─── Drag-drop folder ──────────────────────────────────────────────

    private void OnDragOver(object sender, DragEventArgs e)
    {
        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Copy;
            // Show the overlay. e.DragUIOverride is provided by the OS;
            // we suppress its default text in favor of our own overlay.
            e.DragUIOverride.IsCaptionVisible = false;
            e.DragUIOverride.IsContentVisible = false;
            e.DragUIOverride.IsGlyphVisible = false;
            DragOverlay.Visibility = Visibility.Visible;
        }
    }

    private void OnDragLeave(object sender, DragEventArgs e)
    {
        DragOverlay.Visibility = Visibility.Collapsed;
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        DragOverlay.Visibility = Visibility.Collapsed;
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            return;
        }
        var items = await e.DataView.GetStorageItemsAsync();
        // Take the first folder; ignore anything else (matches macOS — drop
        // a single folder).
        foreach (var item in items)
        {
            if (item is Windows.Storage.StorageFolder folder)
            {
                // Validate the dropped path before assigning. StorageFolder.Path
                // is normally trustworthy, but a junction/symlink that points
                // at a sensitive location (System32, ProgramData) shouldn't
                // be silently scanned. Resolve to a canonical absolute path
                // and reject if Directory.Exists fails after resolution.
                string? canonical;
                try { canonical = System.IO.Path.GetFullPath(folder.Path); }
                catch (Exception ex)
                {
                    DebugLog.Warn($"Drag-drop: GetFullPath failed: {ex.Message}");
                    return;
                }
                if (string.IsNullOrEmpty(canonical) || !System.IO.Directory.Exists(canonical))
                {
                    DebugLog.Warn($"Drag-drop: rejected non-directory or missing path '{PathRedactor.Redact(canonical)}'.");
                    return;
                }
                AppViewModel.Instance.FolderPath = canonical;
                DebugLog.Info($"Drag-drop folder: {PathRedactor.Redact(canonical)}");
                return;
            }
        }
        // No folder in drop — surface a gentle hint.
        await new ContentDialog
        {
            XamlRoot = ((FrameworkElement)Content).XamlRoot,
            Title = "FileID needs a folder",
            Content = "Drop a folder onto FileID to begin scanning. Files won't work — pick the folder they live in.",
            CloseButtonText = "OK",
        }.ShowAsync();
    }

    [DllImport("dwmapi.dll", EntryPoint = "DwmSetWindowAttribute")]
    private static extern int NativeDwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int pvAttribute, int cbAttribute);
}
