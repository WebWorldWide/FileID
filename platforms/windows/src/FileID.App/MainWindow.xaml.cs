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
        InitializeComponent();
        Title = "FileID";

        ApplyTitleBarChrome();
        ApplyMinimumSize();
        ApplySystemBackdrop();
        ForceDarkTitleBar();
        WireKeyboardShortcuts();

        Activated += OnActivated;
        Closed += OnClosed;
        ((FrameworkElement)Content).ActualThemeChanged += OnThemeChanged;

        AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged;
        ApplySidebarVisibility();

        // First-launch model installer. Async-launched after the window
        // has had a moment to layout so the user sees the chrome before
        // the modal pops over it.
        ((FrameworkElement)Content).Loaded += async (_, _) => await MaybeShowWelcomeSheetAsync();
    }

    private async Task MaybeShowWelcomeSheetAsync()
    {
        ModelInstallerService.Instance.Refresh();
        if (ModelInstallerService.Instance.AllInstalled)
        {
            return;
        }

        var sheet = new Views.WelcomeSheet();
        var dialog = new ContentDialog
        {
            XamlRoot = ((FrameworkElement)Content).XamlRoot,
            Content = sheet,
        };
        sheet.Dismissed += (_, _) =>
        {
            try { dialog.Hide(); } catch { }
        };
        try
        {
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Welcome sheet: " + ex.Message);
        }
    }

    private void ApplyTitleBarChrome()
    {
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
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

        // F1 — show keyboard shortcut cheat sheet. Standard Windows help key.
        AddAccelerator(VirtualKey.F1, VirtualKeyModifiers.None, async (_, _) =>
        {
            await ShowShortcutsAsync();
        });
        // Ctrl+? (typed via Ctrl+Shift+/) — alternate "show help" gesture.
        AddAccelerator((VirtualKey)191 /* OEM_2 = '/' on US layouts */,
            VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift,
            async (_, _) => await ShowShortcutsAsync());
    }

    private async Task ShowShortcutsAsync()
    {
        var dialog = new ContentDialog
        {
            XamlRoot = (Content as FrameworkElement)?.XamlRoot,
            Title = "Keyboard shortcuts",
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Close,
            Content = new Views.ShortcutsCheatSheet(),
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
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
                AppViewModel.Instance.FolderPath = folder.Path;
                DebugLog.Info($"Drag-drop folder: {PathRedactor.Redact(folder.Path)}");
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
