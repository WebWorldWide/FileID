// ThemedSegmentedControl — gold-pill segmented control. The Windows analog
// of platforms/apple/app/Sources/FileID/Theme/Theme.swift:141.
//
// Why not <Pivot> or <SelectorBar>: WinUI's stock styles use accent colors
// that don't match our gold token, and re-templating their internals to
// hit the precise visual is more code than rolling our own ItemsControl.
//
// Items are simple (Tag, Label) tuples bound to ItemsSource. SelectedTag
// is the two-way value the host binds to.

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
    /// Binds to a sequence of <see cref="SegmentedOption"/>. Other shapes
    /// (anonymous tuples, plain strings) are not supported; the data
    /// contract is intentionally narrow.
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
