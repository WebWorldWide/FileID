// SettingToggleRow code-behind — three DPs: Title, Subtitle (optional),
// IsOn (TwoWay-bindable). Mirrors the macOS UX exactly: tapping anywhere
// in the row OR on the toggle flips the value.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace FileID.Theme.Controls;

public sealed partial class SettingToggleRow : UserControl
{
    public static readonly DependencyProperty TitleProperty =
        DependencyProperty.Register(nameof(Title), typeof(string), typeof(SettingToggleRow),
            new PropertyMetadata(string.Empty, (d, e) => ((SettingToggleRow)d).TitleText.Text = (string)e.NewValue));

    public static readonly DependencyProperty SubtitleProperty =
        DependencyProperty.Register(nameof(Subtitle), typeof(string), typeof(SettingToggleRow),
            new PropertyMetadata(string.Empty, (d, e) => ((SettingToggleRow)d).ApplySubtitle((string)e.NewValue)));

    public static readonly DependencyProperty IsOnProperty =
        DependencyProperty.Register(nameof(IsOn), typeof(bool), typeof(SettingToggleRow),
            new PropertyMetadata(false, (d, e) =>
            {
                var row = (SettingToggleRow)d;
                if (row.Switch.IsOn != (bool)e.NewValue)
                {
                    row.Switch.IsOn = (bool)e.NewValue;
                }
            }));

    public SettingToggleRow()
    {
        InitializeComponent();
        // Whole-row tap target → toggle. Keeps macOS UX where the row label
        // area is also a tap target, not just the switch.
        Tapped += OnRowTapped;
    }

    public string Title
    {
        get => (string)GetValue(TitleProperty);
        set => SetValue(TitleProperty, value);
    }

    public string Subtitle
    {
        get => (string)GetValue(SubtitleProperty);
        set => SetValue(SubtitleProperty, value);
    }

    public bool IsOn
    {
        get => (bool)GetValue(IsOnProperty);
        set => SetValue(IsOnProperty, value);
    }

    private void ApplySubtitle(string text)
    {
        SubtitleText.Text = text;
        SubtitleText.Visibility = string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible;
    }

    private void OnRowTapped(object sender, TappedRoutedEventArgs e)
    {
        // Don't double-fire if the tap landed on the switch itself.
        if (e.OriginalSource is FrameworkElement fe && IsDescendantOf(fe, Switch))
        {
            return;
        }
        Switch.IsOn = !Switch.IsOn;
    }

    private void OnSwitchToggled(object sender, RoutedEventArgs e)
    {
        // Push the toggle's value out to bindings.
        if (IsOn != Switch.IsOn)
        {
            IsOn = Switch.IsOn;
        }
    }

    private static bool IsDescendantOf(DependencyObject? node, DependencyObject ancestor)
    {
        while (node is not null)
        {
            if (ReferenceEquals(node, ancestor))
            {
                return true;
            }
            node = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(node);
        }
        return false;
    }
}
