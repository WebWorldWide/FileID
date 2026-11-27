using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Input;
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
    private const int MinWidth = 1200;
    private const int MinHeight = 800;

    private const int LaunchWidth = 1480;
    private const int LaunchHeight = 980;

    private SystemBackdropConfiguration? _backdropConfig;
    private MicaController? _micaController;
    private DesktopAcrylicController? _acrylicController;
    private bool _closeSequenceRunning;
    private bool _closeFinalized;

    // WinUI rescales content on a DPI change but not the window itself, so the HWND is subclassed to apply the
    // OS-suggested rect from WM_DPICHANGED. The delegate is held in a field so the GC can't collect it while pinned.
    private SubclassProc? _dpiSubclassProc;
    private IntPtr _subclassedHwnd = IntPtr.Zero;
    private const uint DpiSubclassId = 0xF11E1D;

    private const double SidebarWidth = 232;

    public MainWindow()
    {
        // Each constructor step is wrapped so one failure (e.g. backdrop unsupported on older Win10) doesn't stop the window showing.
        var traceLogPath = System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
            "FileID", "logs", "startup-trace.txt");
        // Trace lines are buffered and flushed once; per-line file appends slowed cold start on slow disks.
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

        // InitializeComponent must succeed; without it there is no window, so let it propagate.
        Trace("InitializeComponent");
        try
        {
            InitializeComponent();
        }
        catch (System.Exception ex)
        {
            Trace($"InitializeComponent failed (HRESULT=0x{ex.HResult:X8}): {ex}");
            FlushTrace();
            throw;
        }
        Title = "FileID";

        Step("ApplyTitleBarChrome", ApplyTitleBarChrome);
        Step("ApplyMinimumSize", ApplyMinimumSize);
        Step("ApplySystemBackdrop", ApplySystemBackdrop);
        Step("ForceDarkTitleBar", ForceDarkTitleBar);
        Step("HookDpiChange", HookDpiChange);
        Step("WireKeyboardShortcuts", WireKeyboardShortcuts);

        Activated += OnActivated;
        Closed += OnClosed;
        AppWindow.Closing += OnAppWindowClosing;
        Step("ThemeChanged subscribe", () => ((FrameworkElement)Content).ActualThemeChanged += OnThemeChanged);

        Step("AppViewModel subscribe", () => AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged);
        Step("ApplySidebarVisibility", ApplySidebarVisibility);
        Step("ApplyAppearance", () =>
        {
            AppearanceApplier.Apply(AppViewModel.Instance.Settings);
            TextScaler.Instance.Attach(DetailHost);
        });

        // Show the welcome sheet after first layout so the chrome appears first; AppWindow.Show(true) makes sure the window is up.
        Step("Welcome subscribe", () =>
            ((FrameworkElement)Content).Loaded += async (_, _) =>
            {
                try
                {
                    AppWindow.Show(true);
                    if (AppWindow.Presenter is OverlappedPresenter p)
                    {
                        p.Restore();
                    }
                }
                catch { }
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
        DebugLog.Info($"[INSTALL] sentinel state: clip={svc.Clip.Status} ramplus={svc.RamPlus.Status} arcface={svc.Arcface.Status} deep_vlm={svc.DeepVlm.Status}");

        // Show on first launch, and on later launches whenever a core model (CLIP/RAM++/face) is missing. The opt-in
        // Deep Analyze VLM is excluded so it can't re-nag. A user who has never seen Welcome must see it even when
        // model files already exist.
        var seen = false;
        try { seen = AppViewModel.Instance.Settings.WelcomeSheetSeen; }
        catch (Exception ex) { DebugLog.Warn("MaybeShowWelcomeSheet: read WelcomeSheetSeen threw: " + ex.Message); }
        if (seen)
        {
            DebugLog.Info("[INSTALL] welcomeSheetSeen=true; skipping welcome sheet.");
            return;
        }

        // An inline overlay confined to Row 1, not a ContentDialog: the dialog's smoke layer would cover the title bar and block dragging.
        DebugLog.Info("[INSTALL] constructing WelcomeSheet + inline overlay.");
        var sheet = new Views.WelcomeSheet();
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        void OnDismissed(object? _, EventArgs __)
        {
            DebugLog.Info("[INSTALL] WelcomeSheet.Dismissed fired; closing overlay.");
            // Marshal to the UI thread: touching Visibility or Content off-thread fast-fails the process.
            var dq = DispatcherQueue;
            if (dq is not null && !dq.HasThreadAccess)
            {
                dq.TryEnqueue(() => OnDismissedOnUi());
            }
            else
            {
                OnDismissedOnUi();
            }

            void OnDismissedOnUi()
            {
                try
                {
                    if (WelcomeOverlay != null)
                    {
                        WelcomeOverlay.Visibility = Microsoft.UI.Xaml.Visibility.Collapsed;
                    }
                    if (WelcomeOverlayHost != null)
                    {
                        WelcomeOverlayHost.Content = null;
                    }
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[INSTALL] overlay teardown threw: " + ex.Message);
                }
                tcs.TrySetResult(true);
            }
        }

        try
        {
            sheet.Dismissed += OnDismissed;
            WelcomeOverlayHost.Content = sheet;
            WelcomeOverlay.Visibility = Microsoft.UI.Xaml.Visibility.Visible;
            DebugLog.Info("[INSTALL] WelcomeOverlay shown; awaiting Dismissed.");
            await tcs.Task.ConfigureAwait(true);
            DebugLog.Info("[INSTALL] WelcomeOverlay dismissed; returning.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[INSTALL] Welcome overlay flow threw: " + ex);
        }
        finally
        {
            try { sheet.Dismissed -= OnDismissed; } catch { /* swallow */ }
        }
    }

    private void ApplyTitleBarChrome()
    {
        ExtendsContentIntoTitleBar = true;
        // SetTitleBar registers a zero-bounds drag region before layout, so wait for Loaded.
        AppTitleBar.Loaded += (_, _) =>
        {
            SetTitleBar(AppTitleBar);
            UpdateTitleBarRegions();
            if (TitleBarHost.XamlRoot is { } root)
            {
                root.Changed += (_, _) => DispatcherQueue.TryEnqueue(UpdateTitleBarRegions);
            }
        };
        TitleBarHost.InteractiveLayoutChanged += (_, _) => DispatcherQueue.TryEnqueue(UpdateTitleBarRegions);

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

    private void UpdateTitleBarRegions()
    {
        try
        {
            if (_closeFinalized || TitleBarHost?.XamlRoot is not { } root) return;
            var scale = root.RasterizationScale;
            if (scale <= 0) scale = 1;
            var rightInset = AppWindow.TitleBar.RightInset;
            TitleBarHost.SetCaptionInset(rightInset > 0 ? rightInset / scale : 138);
            TitleBarHost.UpdateLayout();

            var rects = new List<Windows.Graphics.RectInt32>();
            foreach (var element in TitleBarHost.InteractiveElements)
            {
                if (element.Visibility != Visibility.Visible || element.ActualWidth <= 0 || element.ActualHeight <= 0) continue;
                var bounds = element.TransformToVisual(Content)
                    .TransformBounds(new Rect(0, 0, element.ActualWidth, element.ActualHeight));
                rects.Add(new Windows.Graphics.RectInt32(
                    (int)Math.Round(bounds.X * scale),
                    (int)Math.Round(bounds.Y * scale),
                    (int)Math.Round(bounds.Width * scale),
                    (int)Math.Round(bounds.Height * scale)));
            }
            InputNonClientPointerSource.GetForWindowId(AppWindow.Id)
                .SetRegionRects(NonClientRegionKind.Passthrough, rects.ToArray());
        }
        catch (Exception ex)
        {
            DebugLog.Warn("UpdateTitleBarRegions failed (non-fatal): " + ex.Message);
        }
    }

    private void ApplyMinimumSize()
    {
        // AppWindow sizes are physical pixels; the constants are DIPs, so scale by the monitor DPI.
        uint dpi = GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        if (dpi == 0) { dpi = 96; }
        double scale = dpi / 96.0;
        int minWPx = (int)(MinWidth * scale);
        int minHPx = (int)(MinHeight * scale);
        int launchWPx = (int)(LaunchWidth * scale);
        int launchHPx = (int)(LaunchHeight * scale);

        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            // Preferred minimums are DIPs, unlike Resize/Move.
            presenter.PreferredMinimumWidth = MinWidth;
            presenter.PreferredMinimumHeight = MinHeight;
            presenter.Restore();
        }

        // Launch at the preferred size, capped at 90% of the work area for small laptops.
        var displayArea = DisplayArea.GetFromWindowId(
            AppWindow.Id,
            DisplayAreaFallback.Primary);
        var work = displayArea.WorkArea;
        int targetW = Math.Min(launchWPx, (int)(work.Width * 0.90));
        int targetH = Math.Min(launchHPx, (int)(work.Height * 0.90));
        targetW = Math.Max(targetW, minWPx);
        targetH = Math.Max(targetH, minHPx);

        AppWindow.Resize(new Windows.Graphics.SizeInt32(targetW, targetH));

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

        // Dispose on a construction-time fault so a half-attached controller never leaks; OnClosed handles normal disposal.
        if (MicaController.IsSupported())
        {
            try
            {
                _micaController = new MicaController { Kind = MicaKind.Base };
                _micaController.AddSystemBackdropTarget(this.As<ICompositionSupportsSystemBackdrop>());
                _micaController.SetSystemBackdropConfiguration(_backdropConfig);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("ApplySystemBackdrop: Mica init failed: " + ex.Message);
                try { _micaController?.Dispose(); } catch { }
                _micaController = null;
            }
        }
        else
        {
            try
            {
                _acrylicController = new DesktopAcrylicController();
                _acrylicController.AddSystemBackdropTarget(this.As<ICompositionSupportsSystemBackdrop>());
                _acrylicController.SetSystemBackdropConfiguration(_backdropConfig);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("ApplySystemBackdrop: Acrylic init failed: " + ex.Message);
                try { _acrylicController?.Dispose(); } catch { }
                _acrylicController = null;
            }
        }
    }

    private void ForceDarkTitleBar()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        int yes = 1;
        _ = NativeDwmSetWindowAttribute(hwnd, attribute: 20, ref yes, sizeof(int));

        if (AppWindowTitleBar.IsCustomizationSupported() && AppWindow?.TitleBar is { } tb)
        {
            tb.PreferredHeightOption = TitleBarHeightOption.Tall;
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

    private void HookDpiChange()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        if (hwnd == IntPtr.Zero) { return; }

        // The delegate must outlive the subclass registration or the next WM_DPICHANGED calls freed memory.
        _dpiSubclassProc = DpiWndProc;
        if (SetWindowSubclass(hwnd, _dpiSubclassProc, DpiSubclassId, IntPtr.Zero))
        {
            _subclassedHwnd = hwnd;
        }
        else
        {
            _dpiSubclassProc = null;
        }
    }

    private IntPtr DpiWndProc(
        IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam, uint subclassId, IntPtr refData)
    {
        if (msg == WM_DPICHANGED && lParam != IntPtr.Zero)
        {
            // lParam points to the OS-suggested window rect for the new monitor; WinUI rescales content itself.
            var suggested = Marshal.PtrToStructure<RECT>(lParam);
            int width = suggested.Right - suggested.Left;
            int height = suggested.Bottom - suggested.Top;
            if (width > 0 && height > 0)
            {
                _ = SetWindowPos(
                    hwnd,
                    IntPtr.Zero,
                    suggested.Left,
                    suggested.Top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE);
            }
            return IntPtr.Zero;
        }

        return DefSubclassProc(hwnd, msg, wParam, lParam);
    }

    private void RemoveDpiHook()
    {
        if (_subclassedHwnd != IntPtr.Zero && _dpiSubclassProc is not null)
        {
            _ = RemoveWindowSubclass(_subclassedHwnd, _dpiSubclassProc, DpiSubclassId);
        }
        _subclassedHwnd = IntPtr.Zero;
        _dpiSubclassProc = null;
    }

    private void WireKeyboardShortcuts()
    {
        AddAccelerator(VirtualKey.O, VirtualKeyModifiers.Control, async (_, args) =>
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var result = await FolderPickerService.PickFolderAsync(hwnd, FolderPickerService.ScanCommitText);
            if (result.Path is not null)
            {
                await AppViewModel.Instance.ChooseFolderAsync(result.Path);
            }
            args.Handled = true;
        });

        // Awaited so engine-not-ready errors reach the log instead of vanishing in a fire-and-forget task.
        AddAccelerator(VirtualKey.R, VirtualKeyModifiers.Control, async (_, args) =>
        {
            if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
            var vm = AppViewModel.Instance;
            if (!vm.HasFolder) return;
            try
            {
                await EngineClient.Instance.StartScanAsync(vm.FolderPath!, vm.FolderDisplay);
            }
            catch (Exception ex)
            {
                Services.DebugLog.Error($"Ctrl+R scan failed: {ex.Message}");
            }
            args.Handled = true;
        });

        AddAccelerator(VirtualKey.S, VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift,
            (_, args) =>
            {
                if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
                AppViewModel.Instance.ToggleSidebar();
                args.Handled = true;
            });

        AddAccelerator(VirtualKey.Z, VirtualKeyModifiers.Control, async (_, args) =>
        {
            if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
            // Undoes the showing page's own newest change; other pages' changes stay in Recent changes.
            if (Services.UndoStack.PageForTab(AppViewModel.Instance.ActiveTab.Id) is not { } page) return;
            if (!Services.UndoStack.Instance.CanUndoOn(page)) return;
            var label = await Services.UndoStack.Instance.UndoAsync(page);
            args.Handled = true;
            if (!string.IsNullOrEmpty(label))
            {
                Services.DebugLog.Info($"Undid: {label}");
            }
        });

        // Ctrl+, opens Settings. VirtualKey.Decimal is the numpad period; the comma key is VK_OEM_COMMA (0xBC).
        AddAccelerator((VirtualKey)0xBC, VirtualKeyModifiers.Control,
            (_, args) =>
            {
                if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
                AppViewModel.Instance.ActiveTab = SidebarTab.Settings;
                args.Handled = true;
            });

        AddAccelerator(VirtualKey.K, VirtualKeyModifiers.Control,
            (_, args) =>
            {
                TitleBarHost.FocusSearch();
                args.Handled = true;
            });

        // Ctrl+F raises SearchFocusRequested for the Library view.
        AddAccelerator(VirtualKey.F, VirtualKeyModifiers.Control,
            (_, args) =>
            {
                if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
                if (SearchFocusRequested is null) return;
                SearchFocusRequested.Invoke(this, EventArgs.Empty);
                args.Handled = true;
            });

        // Ctrl+1..9 jump to pages in sidebar order; Alt+1..9 is the Windows-native alias.
        for (int i = 0; i < SidebarTab.ShortcutOrder.Count && i < 9; i++)
        {
            int idx = i;
            var key = (VirtualKey)((int)VirtualKey.Number1 + i);
            foreach (var modifier in new[] { VirtualKeyModifiers.Control, VirtualKeyModifiers.Menu })
            {
                AddAccelerator(key, modifier, (_, args) =>
                {
                    if (KeyboardFocusGuard.IsTextEditing(Content.XamlRoot)) return;
                    AppViewModel.Instance.ActiveTab = SidebarTab.ShortcutOrder[idx];
                    args.Handled = true;
                });
            }
        }

    }

    private void AddAccelerator(VirtualKey key, VirtualKeyModifiers modifiers,
                                TypedEventHandler<KeyboardAccelerator, KeyboardAcceleratorInvokedEventArgs> handler)
    {
        var accel = new KeyboardAccelerator
        {
            Key = key,
            Modifiers = modifiers,
            ScopeOwner = null,
        };
        accel.Invoked += handler;
        ((FrameworkElement)Content).KeyboardAccelerators.Add(accel);
    }

    public event EventHandler? SearchFocusRequested;

    private void OnActivated(object sender, WindowActivatedEventArgs e)
    {
        if (_backdropConfig is not null)
        {
            _backdropConfig.IsInputActive = e.WindowActivationState != WindowActivationState.Deactivated;
        }
    }

    private void OnAppWindowClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_closeFinalized) return;
        args.Cancel = true;
        if (_closeSequenceRunning) return;
        _closeSequenceRunning = true;
        _ = RunCloseSequenceAsync();
    }

    private async Task RunCloseSequenceAsync()
    {
        CloseStopLease? closeStop = null;
        try
        {
            var proceed = Services.ChangeLog.Instance.ActiveUndoCount == 0;
            if (!proceed)
            {
                try
                {
                    var dialog = new ContentDialog
                    {
                        XamlRoot = ((FrameworkElement)Content).XamlRoot,
                        Title = "FileID is still undoing changes",
                        Content = "Wait for the undo in progress to finish before closing FileID.",
                        PrimaryButtonText = "Check again",
                        CloseButtonText = "Keep FileID open",
                        DefaultButton = ContentDialogButton.Close,
                    };
                    proceed = await dialog.ShowAsync() == ContentDialogResult.Primary;
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("Close-confirm dialog failed: " + ex.Message);
                    proceed = false;
                }
            }

            if (!proceed) return;

            closeStop = await StopForApplicationCloseAsync();
            await DrainCloseDispatcherAsync();
            while (Services.ChangeLog.Instance.ActiveUndoCount > 0)
            {
                await closeStop.AbortAsync();
                closeStop = null;
                await ShowCloseBlockedAsync();
                return;
            }

            if (!closeStop.TryCommit())
            {
                await closeStop.AbortAsync();
                closeStop = null;
                await ShowCloseBlockedAsync();
                return;
            }

            _closeFinalized = true;
            Close();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Application close sequence failed: " + ex.Message);
            if (closeStop is not null)
            {
                try { await closeStop.AbortAsync(); }
                catch (Exception restartError) { DebugLog.Warn("Engine restart after cancelled close failed: " + restartError.Message); }
                closeStop = null;
            }
            await ShowCloseBlockedAsync();
        }
        finally
        {
            if (closeStop is not null)
            {
                try { await closeStop.AbortAsync(); }
                catch (Exception restartError) { DebugLog.Warn("Engine restart after close failure failed: " + restartError.Message); }
            }
            _closeSequenceRunning = false;
        }
    }

    private async Task<CloseStopLease> StopForApplicationCloseAsync()
    {
        var engine = EngineClient.Instance;
        var stopped = await engine.StopAndWaitForExitAsync(TimeSpan.FromSeconds(10));
        return new CloseStopLease(engine, stopped);
    }

    private async Task DrainCloseDispatcherAsync()
    {
        if (DispatcherQueue.HasThreadAccess)
        {
            await Task.Yield();
            return;
        }

        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!DispatcherQueue.TryEnqueue(() => completion.TrySetResult()))
        {
            throw new InvalidOperationException("The UI dispatcher stopped before close could be confirmed.");
        }
        await completion.Task;
    }

    private async Task ShowCloseBlockedAsync()
    {
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = ((FrameworkElement)Content).XamlRoot,
                Title = "FileID is still working",
                Content = "FileID could not prove that pending actions and the engine had stopped cleanly. The window will stay open.",
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Close-blocked dialog failed: " + ex.Message);
        }
    }

    private sealed class CloseStopLease(EngineClient engine, bool stopped)
    {
        private bool _committed;
        private bool _abortAttempted;

        public bool TryCommit()
        {
            _committed = stopped;
            return _committed;
        }

        public async Task AbortAsync()
        {
            if (_committed || !stopped || _abortAttempted) return;
            _abortAttempted = true;
            await engine.StartAsync();
        }
    }

    /// <summary>Returns true when the close should proceed after this sequence.</summary>
    private void OnClosed(object sender, WindowEventArgs e)
    {
        AppWindow.Closing -= OnAppWindowClosing;
        if (_micaController is not null) { _micaController.Dispose(); _micaController = null; }
        if (_acrylicController is not null) { _acrylicController.Dispose(); _acrylicController = null; }
        _backdropConfig = null;
        try { RemoveDpiHook(); } catch { /* swallow */ }
        AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;

        try { _ = EngineClient.Instance.ShutdownAsync(); } catch { }

        // Flush the debounced settings save before exit.
        try { AppViewModel.Instance.Settings.SaveImmediately(); } catch { /* swallow */ }

        // Lets the next launch tell this session ended without a native fast-fail.
        try { DebugLog.MarkCleanExit(); } catch { /* swallow */ }
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
        // The named elements only exist between InitializeComponent and Close.
        if (SidebarColumn is null || SidebarHost is null)
        {
            return;
        }
        var visible = AppViewModel.Instance.SidebarVisible;
        if (visible)
        {
            // Restore MinWidth first or the Width assignment is clamped.
            SidebarColumn.MinWidth = SidebarWidth;
            SidebarColumn.Width = new GridLength(SidebarWidth);
            var wasHidden = SidebarHost.Visibility != Visibility.Visible;
            SidebarHost.Visibility = Visibility.Visible;
            if (wasHidden && !ReducedMotion.Instance.IsReduced)
            {
                Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(SidebarHost).Opacity = 0f;
                SpringEasing.AnimateOpacity(SidebarHost, 1f, 0.38, 0.8);
            }
        }
        else
        {
            // Clear MinWidth first: it overrides Width=0 and the column would stay wide.
            SidebarColumn.MinWidth = 0;
            SidebarColumn.Width = new GridLength(0);
            SidebarHost.Visibility = Visibility.Collapsed;
        }
    }

    private void OnDragOver(object sender, DragEventArgs e)
    {
        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Copy;
            // DragUIOverride can be null for some drag sources.
            if (e.DragUIOverride is { } dragUi)
            {
                dragUi.IsCaptionVisible = false;
                dragUi.IsContentVisible = false;
                dragUi.IsGlyphVisible = false;
            }
            DragOverlay.Visibility = Visibility.Visible;
        }
    }

    private void OnDragLeave(object sender, DragEventArgs e)
    {
        DragOverlay.Visibility = Visibility.Collapsed;
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        // Async void: an escaping exception would crash the dispatcher, so every failure is caught and logged.
        try
        {
            await OnDropCoreAsync(e);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnDrop threw: " + ex.Message);
        }
    }

    private async Task OnDropCoreAsync(DragEventArgs e)
    {
        DragOverlay.Visibility = Visibility.Collapsed;
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            return;
        }
        var items = await e.DataView.GetStorageItemsAsync();
        foreach (var item in items)
        {
            if (item is Windows.Storage.StorageFolder folder)
            {
                // Reject non-directories after canonicalizing the path.
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
                // Junctions and symlinks could redirect a scan into a sensitive location, so they are refused.
                try
                {
                    var attrs = System.IO.File.GetAttributes(canonical);
                    if ((attrs & System.IO.FileAttributes.ReparsePoint) != 0)
                    {
                        DebugLog.Warn($"Drag-drop: rejected reparse point '{PathRedactor.Redact(canonical)}'.");
                        try
                        {
                            await new ContentDialog
                            {
                                XamlRoot = ((FrameworkElement)Content).XamlRoot,
                                Title = "Can't scan a symlink or junction",
                                Content = "FileID won't scan a folder that's a symlink or junction — please pick the real folder it points to.",
                                CloseButtonText = "OK",
                            }.ShowAsync();
                        }
                        catch { /* dialog already open / broken XamlRoot */ }
                        return;
                    }
                }
                catch (Exception ex)
                {
                    DebugLog.Warn($"Drag-drop: File.GetAttributes failed: {ex.Message}");
                    return;
                }
                DebugLog.Info($"Drag-drop folder: {PathRedactor.Redact(canonical)}");
                await AppViewModel.Instance.ChooseFolderAsync(canonical);
                return;
            }
        }
        try
        {
            await new ContentDialog
            {
                XamlRoot = ((FrameworkElement)Content).XamlRoot,
                Title = "FileID needs a folder",
                Content = "Drop a folder onto FileID to begin scanning. Files won't work — pick the folder they live in.",
                CloseButtonText = "OK",
            }.ShowAsync();
        }
        catch { /* dialog already open / broken XamlRoot */ }
    }

    [DllImport("dwmapi.dll", EntryPoint = "DwmSetWindowAttribute")]
    private static extern int NativeDwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int pvAttribute, int cbAttribute);

    private const uint WM_DPICHANGED = 0x02E0;
    private const uint SWP_NOZORDER = 0x0004;
    private const uint SWP_NOACTIVATE = 0x0010;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate IntPtr SubclassProc(
        IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam, uint subclassId, IntPtr refData);

    [DllImport("comctl32.dll", SetLastError = true)]
    private static extern bool SetWindowSubclass(
        IntPtr hwnd, SubclassProc callback, uint subclassId, IntPtr refData);

    [DllImport("comctl32.dll", SetLastError = true)]
    private static extern bool RemoveWindowSubclass(
        IntPtr hwnd, SubclassProc callback, uint subclassId);

    [DllImport("comctl32.dll")]
    private static extern IntPtr DefSubclassProc(
        IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(
        IntPtr hwnd, IntPtr hwndInsertAfter, int x, int y, int cx, int cy, uint flags);
}
