// SidebarEngineStatus code-behind. Subscribes to EngineClient lifecycle and re-paints the status pill.

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
                DebugLog.Info($"[ENGINE-SUB:SidebarEngineStatus] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(Sync);
            }
        });

    private void Sync()
    {
        var ec = EngineClient.Instance;
        var healthy = ec.State == EngineClient.LifecycleState.Ready && ec.LastError is null;
        Visibility = healthy ? Visibility.Collapsed : Visibility.Visible;
        if (healthy) return;

        switch (ec.State)
        {
            case EngineClient.LifecycleState.Starting:
                ApplyStatus(StatusAccent.Gold, "Getting ready…", "FileID is starting its background helper. This takes a few seconds.");
                break;
            case EngineClient.LifecycleState.Ready:
                // Ready but with a recent error — show red so the user notices.
                ApplyStatus(StatusAccent.Red,
                    EngineErrorText.Title(ec.LastError?.Kind),
                    EngineErrorText.Text(ec.LastError?.Kind, ec.LastError?.Message));
                break;
            case EngineClient.LifecycleState.Crashed:
                ApplyStatus(StatusAccent.Red,
                    ResolveCrashedStatusText(ec.CrashReason),
                    ResolveCrashedStatusTip(ec.CrashReason));
                break;
        }
    }

    private enum StatusAccent { Gold, Red }

    internal static string ResolveCrashedStatusText(string? reason) =>
        reason == EngineClient.StoppedReason ? "FileID's helper is off" : "FileID's helper stopped";

    // The tooltip may carry the technical reason; the label never does.
    internal static string ResolveCrashedStatusTip(string? reason) =>
        string.IsNullOrWhiteSpace(reason)
            ? "FileID's helper stopped unexpectedly. Quit and reopen FileID."
            : reason == EngineClient.StoppedReason
                ? "FileID's helper is off. Turn it back on in Settings."
                : "FileID's helper stopped. Quit and reopen FileID. Details: " + reason;

    /// <summary>Brushes are pre-cached at ctor (see field declarations above) to avoid the DispatcherObject construction race on view teardown.</summary>
    private void ApplyStatus(StatusAccent accent, string text, string tip)
    {
        StatusDot.Fill = accent == StatusAccent.Gold ? _goldDot : _redDot;
        StatusGlow.Fill = accent == StatusAccent.Gold ? _goldGlow : _redGlow;
        StatusText.Text = text;
        ToolTipService.SetToolTip(this, tip);
        // Screen-reader name for the status pill — the live engine state plus its detail line, so the dot+text reads as one announcement.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(this, $"FileID status: {text}");
    }
}
