// SidebarEngineStatus code-behind. Subscribes to EngineClient lifecycle
// and re-paints the pill — including the soft glow ring around the dot
// for that "live status" feel.

using System.ComponentModel;
using FileID.ViewModels;
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
    {
        if (e.PropertyName is nameof(EngineClient.State)
                          or nameof(EngineClient.Info)
                          or nameof(EngineClient.CrashReason))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    private void Sync()
    {
        var ec = EngineClient.Instance;
        switch (ec.State)
        {
            case EngineClient.LifecycleState.Starting:
                ApplyStatus(0xFF, 0xCC, 0x00, "Engine starting…", "Engine is launching.");
                break;
            case EngineClient.LifecycleState.Ready:
                ApplyStatus(0x6B, 0xE0, 0x82, "Engine ready", BuildReadyTooltip(ec));
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
