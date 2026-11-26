// ThemedSegmentedControl — gold-pill segmented control.
//
// Not <Pivot> or <SelectorBar>: their stock styles use accent colors that
// don't match the gold token, and re-templating them is more code than this ItemsControl.
//
// Items are SegmentedOption values bound to ItemsSource; SelectedTag is the
// two-way value the host binds to.

using System.Collections;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Theme.Controls;

public sealed class ThemedSegmentedControl : Control
{
    public static readonly DependencyProperty ItemsSourceProperty =
        DependencyProperty.Register(nameof(ItemsSource), typeof(IEnumerable), typeof(ThemedSegmentedControl),
            new PropertyMetadata(null));

    public static readonly DependencyProperty SelectedTagProperty =
        DependencyProperty.Register(nameof(SelectedTag), typeof(string), typeof(ThemedSegmentedControl),
            new PropertyMetadata(string.Empty));

    public ThemedSegmentedControl()
    {
        DefaultStyleKey = typeof(ThemedSegmentedControl);
    }

    /// <summary>
    /// Binds to a sequence of <see cref="SegmentedOption"/>; other shapes are not supported.
    /// </summary>
    public IEnumerable? ItemsSource
    {
        get => (IEnumerable?)GetValue(ItemsSourceProperty);
        set => SetValue(ItemsSourceProperty, value);
    }

    /// <summary>
    /// The currently-selected option's <see cref="SegmentedOption.Tag"/>.
    /// Two-way bindable.
    /// </summary>
    public string SelectedTag
    {
        get => (string)GetValue(SelectedTagProperty);
        set => SetValue(SelectedTagProperty, value);
    }
}

/// <summary>
/// Single segment in a <see cref="ThemedSegmentedControl"/>. Tag is the
/// stable identifier the host binds against; Label is the display text.
/// </summary>
// Plain set accessors (not init-only) so the WinUI XAML compiler's x:Bind
// path resolution works — XamlCompiler.exe's .NET Framework runtime can't
// resolve the init modreq IsExternalInit and silently exits 1.
public sealed class SegmentedOption
{
    public string Tag { get; set; } = string.Empty;
    public string Label { get; set; } = string.Empty;
}
