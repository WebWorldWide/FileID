// ThemedTogglePicker code-behind. Three DPs (FalseLabel, TrueLabel,
// Selection) plus pill-fill state mirroring on click.
//
// Why not a stock ToggleSwitch with custom labels: ToggleSwitch's chrome
// is a sliding thumb, fundamentally different from the segmented-pill
// look macOS uses for these binary mode toggles. Visual fidelity wins.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Theme.Controls;

public sealed partial class ThemedTogglePicker : UserControl
{
    public static readonly DependencyProperty FalseLabelProperty =
        DependencyProperty.Register(nameof(FalseLabel), typeof(string), typeof(ThemedTogglePicker),
            new PropertyMetadata(string.Empty, (d, e) => ((ThemedTogglePicker)d).FalseButton.Content = (string)e.NewValue));

    public static readonly DependencyProperty TrueLabelProperty =
        DependencyProperty.Register(nameof(TrueLabel), typeof(string), typeof(ThemedTogglePicker),
            new PropertyMetadata(string.Empty, (d, e) => ((ThemedTogglePicker)d).TrueButton.Content = (string)e.NewValue));

    public static readonly DependencyProperty SelectionProperty =
        DependencyProperty.Register(nameof(Selection), typeof(bool), typeof(ThemedTogglePicker),
            new PropertyMetadata(false, (d, _) => ((ThemedTogglePicker)d).RefreshFills()));

    public ThemedTogglePicker()
    {
        InitializeComponent();
        RefreshFills();
    }

    public string FalseLabel
    {
        get => (string)GetValue(FalseLabelProperty);
        set => SetValue(FalseLabelProperty, value);
    }

    public string TrueLabel
    {
        get => (string)GetValue(TrueLabelProperty);
        set => SetValue(TrueLabelProperty, value);
    }

    public bool Selection
    {
        get => (bool)GetValue(SelectionProperty);
        set => SetValue(SelectionProperty, value);
    }

    private void OnFalseClicked(object sender, RoutedEventArgs e) => Selection = false;
    private void OnTrueClicked(object sender, RoutedEventArgs e) => Selection = true;

    private void RefreshFills()
    {
        ApplyState(FalseButton, isActive: !Selection);
        ApplyState(TrueButton, isActive: Selection);
    }

    private static void ApplyState(Button btn, bool isActive)
    {
        if (isActive)
        {
            // Resolve the GoldColor at runtime so we don't capture a stale
            // reference if the theme dictionary is hot-swapped (future).
            if (Application.Current?.Resources["GoldColor"] is Color gold)
            {
                btn.Background = new SolidColorBrush(gold);
            }
            btn.Foreground = new SolidColorBrush(Colors.Black);
            btn.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
        }
        else
        {
            btn.Background = new SolidColorBrush(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF)); // white @ 8%
            btn.Foreground = new SolidColorBrush(Color.FromArgb(0xB3, 0xFF, 0xFF, 0xFF)); // white @ 70%
            btn.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        }
    }
}
