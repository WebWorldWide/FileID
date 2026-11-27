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
// Whenever the trigger changes, a ring sized to the element's bounds is
// shown in a Popup over it and animates out.

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
        // Invoked by the dependency-property system outside any SafeRun scope, so a throw would escape into the dispatcher loop.
        try
        {
            if (d is not FrameworkElement target)
            {
                return;
            }
            // The first set during XAML load must not ripple.
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
                return;
            }
            // An unrooted Popup is a native fast-fail, so require a rooted, laid-out target.
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
        // Popup requires XamlRoot in WinUI 3; opening an unrooted one is a native fast-fail.
        var popup = new Popup
        {
            IsHitTestVisible = false,
            XamlRoot = target.XamlRoot,
        };

        Color goldColor = ResolveGoldColor();

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

        // TransformToVisual(null) is relative to the visual root, the same space the Popup offsets use.
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
