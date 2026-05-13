// AutoPilotTracker code-behind. Mirrors EngineClient.CurrentAutoPilotStage
// to the dot-tracker UI. Pending dots stay faint; the active stage's dot
// goes gold + the label brightens; completed stages go green.

using System.ComponentModel;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.AutoPilot;

public sealed partial class AutoPilotTracker : UserControl
{
    private static readonly SolidColorBrush GoldBrush =
        new(Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
    private static readonly SolidColorBrush GreenBrush =
        new(Color.FromArgb(0xFF, 0x6B, 0xE0, 0x82));

    /// <summary>V14.9-K4: tracks whether Sync() has been observed since the
    /// last stage transition. Used by the DEBUG-only `OnEngineChanged`
    /// instrumentation to log a single "tracker became visible" line per
    /// AutoPilot run — so a future regression that hides the control
    /// from the visual tree shows up immediately in engine.jsonl.</summary>
    private bool _debugLoggedVisible;

    public AutoPilotTracker()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged += OnEngineChanged;
            Sync();
        };
        Unloaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        };
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(EngineClient.CurrentAutoPilotStage))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    private void Sync()
    {
        var stage = EngineClient.Instance.CurrentAutoPilotStage;
        Visibility = stage is null ? Visibility.Collapsed : Visibility.Visible;
        if (stage is null)
        {
            _debugLoggedVisible = false;
            return;
        }
#if DEBUG
        // V14.9-K4: a future regression that detaches the tracker from
        // the visual tree (e.g. wrong column/row index, parent collapsed)
        // would silently keep AutoPilot working but invisible. Log a one-
        // shot debug line per run so the engine.jsonl shows the tracker
        // mounted whenever a stage transition happens.
        if (!_debugLoggedVisible)
        {
            _debugLoggedVisible = true;
            FileID.Services.DebugLog.Info(
                $"[AUTOPILOT-TRACKER] mounted (stage={stage}, parent={Parent?.GetType().Name ?? "<null>"})");
        }
#endif

        // Tier 0..3 corresponds to Scan / Cluster / Caption / Plan. The
        // Complete state lights all four green.
        int activeIdx = stage switch
        {
            EngineClient.AutoPilotStage.Scanning   => 0,
            EngineClient.AutoPilotStage.Clustering => 1,
            EngineClient.AutoPilotStage.Captioning => 2,
            EngineClient.AutoPilotStage.Planning   => 3,
            EngineClient.AutoPilotStage.Complete   => 4,
            _                                       => -1,
        };

        ApplyDot(DotScan,    LabelScan,    0, activeIdx);
        ApplyDot(DotCluster, LabelCluster, 1, activeIdx);
        ApplyDot(DotCaption, LabelCaption, 2, activeIdx);
        ApplyDot(DotPlan,    LabelPlan,    3, activeIdx);
    }

    private void ApplyDot(Microsoft.UI.Xaml.Shapes.Ellipse dot, TextBlock label, int idx, int activeIdx)
    {
        if (idx < activeIdx)
        {
            dot.Fill = GreenBrush;
            label.Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        }
        else if (idx == activeIdx)
        {
            dot.Fill = GoldBrush;
            label.Foreground = (Brush)Application.Current.Resources["TextFillColorPrimaryBrush"];
        }
        else
        {
            dot.Fill = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
            label.Foreground = (Brush)Application.Current.Resources["TextFillColorTertiaryBrush"];
        }
    }
}
