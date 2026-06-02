// SidebarEngineStatus code-behind. Subscribes to EngineClient lifecycle
// and re-paints the pill — including the soft glow ring around the dot
// for that "live status" feel.

using System.ComponentModel;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarEngineStatus : UserControl
{
    // Pre-cached brush pairs. The prior code did `new SolidColorBrush(...)`
    // inside ApplyStatus per call. `SolidColorBrush` is a `DispatcherObject`;
    // naked construction on a teardown-race path (Unloaded firing while a
    // PropertyChanged was already in-flight on the UI dispatcher) is a known
    // native fast-fail shape — see V15.2 / V15.2.1 / V15.4 in CLAUDE.md.
    // Allocating once at ctor time guarantees the brushes were constructed
    // on the dispatcher this control owns.
    private readonly SolidColorBrush _goldDot;
    private readonly SolidColorBrush _goldGlow;
    private readonly SolidColorBrush _redDot;
    private readonly SolidColorBrush _redGlow;

    public SidebarEngineStatus()
    {
        InitializeComponent();
        _goldDot = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        _goldGlow = new SolidColorBrush(Color.FromArgb(0x38, 0xFF, 0xCC, 0x00));
        _redDot = new SolidColorBrush(Color.FromArgb(0xFF, 0xE5, 0x55, 0x55));
        _redGlow = new SolidColorBrush(Color.FromArgb(0x38, 0xE5, 0x55, 0x55));
        Loaded += (_, _) => Sync();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarEngineStatus.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.State)
                              or nameof(EngineClient.Info)
                              or nameof(EngineClient.CrashReason)
                              or nameof(EngineClient.LastError))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarEngineStatus] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(Sync);
            }
        });

    private void Sync()
    {
        var ec = EngineClient.Instance;
        // Hide the pill entirely when everything's healthy — matches macOS
        // Sidebar.swift:103-111. The pill is meant to surface trouble; in
        // the steady state it's just noise.
        var healthy = ec.State == EngineClient.LifecycleState.Ready && ec.LastError is null;
        Visibility = healthy ? Visibility.Collapsed : Visibility.Visible;
        if (healthy) return;

        switch (ec.State)
        {
            case EngineClient.LifecycleState.Starting:
                ApplyStatus(StatusAccent.Gold, "Engine starting…", "Engine is launching.");
                break;
            case EngineClient.LifecycleState.Ready:
                // Ready but with a recent error — show red so the user notices.
                ApplyStatus(StatusAccent.Red,
                    ec.LastError?.Message ?? "Engine reported an error",
                    ec.LastError?.Message ?? "See app.log for details.");
                break;
            case EngineClient.LifecycleState.Crashed:
                ApplyStatus(StatusAccent.Red,
                    ec.CrashReason ?? "Engine crashed",
                    ec.CrashReason ?? "Engine crashed. Check %LOCALAPPDATA%\\FileID\\logs\\app.log.");
                break;
        }
    }

    private enum StatusAccent { Gold, Red }

    /// <summary>
    /// Sets the dot, glow ring, label, and tooltip in one shot. Glow ring
    /// is the same RGB as the dot but at 22% alpha — gives a soft Fluent
    /// "reveal-style" halo without using a real shadow primitive. Brushes
    /// are pre-cached at ctor (see field declarations above) to avoid the
    /// V15.2-class DispatcherObject construction race on view teardown.
    /// </summary>
    private void ApplyStatus(StatusAccent accent, string text, string tip)
    {
        StatusDot.Fill = accent == StatusAccent.Gold ? _goldDot : _redDot;
        StatusGlow.Fill = accent == StatusAccent.Gold ? _goldGlow : _redGlow;
        StatusText.Text = text;
        ToolTipService.SetToolTip(this, tip);
        // Screen-reader name for the status pill — the live engine state
        // plus its detail line, so the dot+text reads as one announcement.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(this, $"Engine status: {text}");
    }

    private static string BuildReadyTooltip(EngineClient ec) =>
        ec.Info is { } info
            ? $"Version {info.Version}  •  PID {info.Pid}  •  {info.WorkerCap} workers  •  {info.PhysicalMemoryGB:F1} GB RAM"
            : "Engine running.";
}
