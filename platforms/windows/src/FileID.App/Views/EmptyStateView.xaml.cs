// EmptyStateView code-behind. Four DPs (Glyph / Title / Body / Secondary).

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views;

public sealed partial class EmptyStateView : UserControl
{
    public static readonly DependencyProperty GlyphProperty =
        DependencyProperty.Register(nameof(Glyph), typeof(string), typeof(EmptyStateView),
            new PropertyMetadata(string.Empty, (d, e) => ((EmptyStateView)d).GlyphIcon.Glyph = (string)e.NewValue));

    public static readonly DependencyProperty TitleProperty =
        DependencyProperty.Register(nameof(Title), typeof(string), typeof(EmptyStateView),
            new PropertyMetadata(string.Empty, (d, e) => ((EmptyStateView)d).TitleText.Text = (string)e.NewValue));

    public static readonly DependencyProperty BodyProperty =
        DependencyProperty.Register(nameof(Body), typeof(string), typeof(EmptyStateView),
            new PropertyMetadata(string.Empty, (d, e) => ((EmptyStateView)d).BodyText.Text = (string)e.NewValue));

    public static readonly DependencyProperty SecondaryProperty =
        DependencyProperty.Register(nameof(Secondary), typeof(string), typeof(EmptyStateView),
            new PropertyMetadata(string.Empty, (d, e) =>
            {
                var v = (EmptyStateView)d;
                var text = (string)e.NewValue;
                v.SecondaryText.Text = text;
                v.SecondaryText.Visibility = string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible;
            }));

    public EmptyStateView()
    {
        InitializeComponent();
    }

    public string Glyph { get => (string)GetValue(GlyphProperty); set => SetValue(GlyphProperty, value); }
    public string Title { get => (string)GetValue(TitleProperty); set => SetValue(TitleProperty, value); }
    public string Body { get => (string)GetValue(BodyProperty); set => SetValue(BodyProperty, value); }
    public string Secondary { get => (string)GetValue(SecondaryProperty); set => SetValue(SecondaryProperty, value); }
}
