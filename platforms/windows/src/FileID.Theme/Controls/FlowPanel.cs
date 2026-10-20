using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace FileID.Theme.Controls;

/// <summary>Left-to-right wrapping panel (macOS FlowLayout): tag chips and
/// citation chips wrap onto new lines with uniform spacing.</summary>
public sealed class FlowPanel : Panel
{
    public static readonly DependencyProperty SpacingProperty = DependencyProperty.Register(
        nameof(Spacing), typeof(double), typeof(FlowPanel),
        new PropertyMetadata(6.0, (d, _) => ((FlowPanel)d).InvalidateMeasure()));

    public double Spacing
    {
        get => (double)GetValue(SpacingProperty);
        set => SetValue(SpacingProperty, value);
    }

    protected override Size MeasureOverride(Size availableSize)
    {
        double maxWidth = double.IsInfinity(availableSize.Width) ? double.MaxValue : availableSize.Width;
        double x = 0, y = 0, lineHeight = 0, widest = 0;
        foreach (var child in Children)
        {
            child.Measure(new Size(maxWidth, double.PositiveInfinity));
            var size = child.DesiredSize;
            if (x > 0 && x + size.Width > maxWidth)
            {
                y += lineHeight + Spacing;
                x = 0;
                lineHeight = 0;
            }
            x += size.Width + Spacing;
            lineHeight = Math.Max(lineHeight, size.Height);
            widest = Math.Max(widest, x - Spacing);
        }
        return new Size(widest, Children.Count == 0 ? 0 : y + lineHeight);
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        double x = 0, y = 0, lineHeight = 0;
        foreach (var child in Children)
        {
            var size = child.DesiredSize;
            if (x > 0 && x + size.Width > finalSize.Width)
            {
                y += lineHeight + Spacing;
                x = 0;
                lineHeight = 0;
            }
            child.Arrange(new Rect(x, y, size.Width, size.Height));
            x += size.Width + Spacing;
            lineHeight = Math.Max(lineHeight, size.Height);
        }
        return finalSize;
    }
}
