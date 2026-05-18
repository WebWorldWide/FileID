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
    public SidebarEngineStatus()
    {
        InitializeComponent();
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
                ApplyStatus(0xFF, 0xCC, 0x00, "Engine starting…", "Engine is launching.");
                break;
            case EngineClient.LifecycleState.Ready:
                // Ready but with a recent error — show red so the user notices.
                ApplyStatus(0xE5, 0x55, 0x55,
                    ec.LastError?.Message ?? "Engine reported an error",
                    ec.LastError?.Message ?? "See app.log for details.");
                break;
            case EngineClient.LifecycleState.Crashed:
                ApplyStatus(0xE5, 0x55, 0x55,
                    ec.CrashReason ?? "Engine crashed",
                    ec.CrashReason ?? "Engine crashed. Check %LOCALAPPDATA%\\FileID\\logs\\app.log.");
                break;
        }
    }

    /// <summary>
    /// Sets the dot, glow ring, label, and tooltip in one shot. Glow ring
    /// is the same RGB as the dot but at 22% alpha — gives a soft Fluent
    /// "reveal-style" halo without using a real shadow primitive.
    /// </summary>
    private void ApplyStatus(byte r, byte g, byte b, string text, string tip)
    {
        StatusDot.Fill = new SolidColorBrush(Color.FromArgb(0xFF, r, g, b));
        StatusGlow.Fill = new SolidColorBrush(Color.FromArgb(0x38, r, g, b));
        StatusText.Text = text;
        ToolTipService.SetToolTip(this, tip);
    }

    private static string BuildReadyTooltip(EngineClient ec) =>
        ec.Info is { } info
            ? $"Version {info.Version}  •  PID {info.Pid}  •  {info.WorkerCap} workers  •  {info.PhysicalMemoryGB:F1} GB RAM"
            : "Engine running.";
}
