// BadgePill — code-behind for the BadgePill UserControl. Two DPs:
//   - Label: the displayed text
//   - Accent: the tint color (a SolidColorBrush; we reach in for the .Color
//     to derive the 15%-opaque background fill)
//
// Default accent is gold; callers override per semantic role (Ai for
// AI-running, Info for informational, Delight for success). Keep this
// control intentionally minimal — anything more is scope creep.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Theme.Controls;

public sealed partial class BadgePill : UserControl
{
    public static readonly DependencyProperty LabelProperty =
        DependencyProperty.Register(nameof(Label), typeof(string), typeof(BadgePill),
            new PropertyMetadata(string.Empty, OnLabelChanged));

    public static readonly DependencyProperty AccentProperty =
        DependencyProperty.Register(nameof(Accent), typeof(SolidColorBrush), typeof(BadgePill),
            new PropertyMetadata(null, OnAccentChanged));

    public BadgePill()
    {
        InitializeComponent();
        // Apply current state once after Initialize so resources are reachable.
        ApplyAccent();
    }

    public string Label
    {
        get => (string)GetValue(LabelProperty);
        set => SetValue(LabelProperty, value);
    }

    /// <summary>
    /// Tint brush. Foreground uses this color directly; background uses
    /// the same color at 15% opacity. Defaults to GoldBrush from Theme.xaml
    /// when null.
    /// </summary>
    public SolidColorBrush? Accent
    {
        get => (SolidColorBrush?)GetValue(AccentProperty);
        set => SetValue(AccentProperty, value);
    }

    private static void OnLabelChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is BadgePill p)
        {
            p.LabelText.Text = (string)e.NewValue;
        }
    }

    private static void OnAccentChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is BadgePill p)
        {
            p.ApplyAccent();
        }
    }

    private void ApplyAccent()
    {
        var color = ResolveAccentColor();
        LabelText.Foreground = new SolidColorBrush(color);
        PillRoot.Background = new SolidColorBrush(WithOpacity(color, 0.15));
    }

    private Color ResolveAccentColor()
    {
        if (Accent is { } b)
        {
            return b.Color;
        }
        // Fallback: pull GoldColor from the merged theme dictionary.
        if (Application.Current?.Resources["GoldColor"] is Color goldColor)
        {
            return goldColor;
        }
        // Last-resort literal — should never hit unless Theme.xaml failed
        // to merge, in which case the rest of the UI is also broken.
        return Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00);
    }

    private static Color WithOpacity(Color c, double alpha)
    {
        var clamped = (byte)Math.Round(Math.Clamp(alpha, 0.0, 1.0) * 255);
        return Color.FromArgb(clamped, c.R, c.G, c.B);
    }
}
