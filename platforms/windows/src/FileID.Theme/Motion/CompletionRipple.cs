// CompletionRipple — a one-shot gold ring pulse used as a "nice job"
// completion affordance.
//
// Scale 0.4 → 2.6, opacity 0.85 → 0 over 0.9s easeOut. Fires on any
// trigger value change (not just false→true). Skips entirely when
// reduced-motion is on.
//
// Usage as an attached behavior:
//
//     <Button x:Name="ApplyButton"
//             motion:CompletionRipple.Trigger="{x:Bind ViewModel.AppliedCount, Mode=OneWay}" />
//
// The trigger is observed; whenever it changes, a fresh ring overlay is
// inserted into the element's parent and animates out. The ring is sized
// to fit the element's bounds.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;

namespace FileID.Theme.Motion;

public static class CompletionRipple
{
    /// <summary>
    /// Attached property used as a trigger. The actual value isn't read —
    /// we fire a ripple on every change.
    /// </summary>
    public static readonly DependencyProperty TriggerProperty =
        DependencyProperty.RegisterAttached(
            "Trigger",
            typeof(object),
            typeof(CompletionRipple),
            new PropertyMetadata(null, OnTriggerChanged));

    public static object? GetTrigger(DependencyObject d) => d.GetValue(TriggerProperty);
    public static void SetTrigger(DependencyObject d, object? value) => d.SetValue(TriggerProperty, value);

    private static void OnTriggerChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        // Hard try/catch. OnTriggerChanged is invoked by the dependency-
        // property system OUTSIDE any subscriber's SafeRun scope — a throw
        // here escapes into the dispatcher loop and (with the prior unrooted-
        // Popup bug) caused a mid-scan native fast-fail.
        try
        {
            if (d is not FrameworkElement target)
            {
                return;
            }
            // First-time set during XAML load: don't fire (would ripple on every
            // page load). Subsequent changes do fire.
            if (e.OldValue is null && e.NewValue is not null)
            {
                return;
            }
            if (Equals(e.OldValue, e.NewValue))
            {
                return;
            }
            if (ReducedMotion.Instance.IsReduced)
            {
                // Honor the OS preference: completion event still happened,
                // just no animated decoration.
                return;
            }
            // Never ripple if the target isn't laid out or rooted in a
            // window. The popup logic below would either no-op silently or
            // try to open an unrooted Popup and trigger a native fast-fail.
            if (target.XamlRoot is null) return;
            if (target.ActualWidth <= 0 || target.ActualHeight <= 0) return;

            FireRipple(target);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"CompletionRipple.OnTriggerChanged threw: {ex}");
        }
    }

    private static void FireRipple(FrameworkElement target)
    {
        // We need a Canvas (or any panel) above `target` to place the ring.
        // The simplest hostable surface is the visual root's Popup layer:
        // construct an Ellipse, attach it to a Popup positioned over the
        // target, animate, then dispose.
        //
        // Popup REQUIRES XamlRoot in WinUI 3 (unlike UWP) — opening an
        // unrooted Popup is a native fast-fail. The OnTriggerChanged guard
        // rejects null XamlRoot above; also set it on the new Popup so a
        // future code path bypassing that check still gets a valid host.
        var popup = new Popup
        {
            IsHitTestVisible = false,
            XamlRoot = target.XamlRoot,
        };

        Color goldColor = ResolveGoldColor();

        // Ring: empty ellipse with a 2px gold stroke. Scaling it from 0.4 to
        // 2.6 sweeps a halo around the target.
        var ring = new Ellipse
        {
            Width = target.ActualWidth,
            Height = target.ActualHeight,
            Stroke = new SolidColorBrush(goldColor),
            StrokeThickness = 2,
            IsHitTestVisible = false,
            RenderTransformOrigin = new Point(0.5, 0.5),
            RenderTransform = new ScaleTransform { ScaleX = 0.4, ScaleY = 0.4 },
            Opacity = 0.85,
        };
        popup.Child = ring;

        // Position the popup so the ring is centered on the target.
        // TransformToVisual(null) returns the transform relative to the
        // visual root — the same coord space the Popup uses when XamlRoot
        // is set, so the offsets line up.
        var transform = target.TransformToVisual(null);
        var topLeft = transform.TransformPoint(new Point(0, 0));
        popup.HorizontalOffset = topLeft.X;
        popup.VerticalOffset = topLeft.Y;
        popup.IsOpen = true;

        var sb = new Storyboard();
        var ease = new ExponentialEase { EasingMode = EasingMode.EaseOut, Exponent = 4 };
        var dur = new Duration(TimeSpan.FromSeconds(0.9));

        var scaleX = new DoubleAnimation { To = 2.6, Duration = dur, EasingFunction = ease };
        Storyboard.SetTarget(scaleX, ring.RenderTransform);
        Storyboard.SetTargetProperty(scaleX, "ScaleX");
        var scaleY = new DoubleAnimation { To = 2.6, Duration = dur, EasingFunction = ease };
        Storyboard.SetTarget(scaleY, ring.RenderTransform);
        Storyboard.SetTargetProperty(scaleY, "ScaleY");
        var opacity = new DoubleAnimation { To = 0, Duration = dur, EasingFunction = ease };
        Storyboard.SetTarget(opacity, ring);
        Storyboard.SetTargetProperty(opacity, "Opacity");

        sb.Children.Add(scaleX);
        sb.Children.Add(scaleY);
        sb.Children.Add(opacity);
        sb.Completed += (_, _) =>
        {
            try { popup.IsOpen = false; } catch { /* popup may already be closed via XamlRoot teardown */ }
        };
        sb.Begin();
    }

    private static Color ResolveGoldColor()
    {
        if (Application.Current?.Resources["GoldColor"] is Color c)
        {
            return c;
        }
        return Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00);
    }
}
