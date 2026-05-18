// TagChip — code-behind for the small auto-tag chip Library cards show
// below the filename. Visual spec mirrors macOS LibraryView.swift:729-744
// (`Color.secondary.opacity(0.10)` fill + secondary foreground, 11 pt
// Medium, 5×2 padding, 4 px corner radius). One DependencyProperty —
// `Tag` — and a static `FormatTag` helper the control invokes itself so
// callers bind raw classifier output without a value converter.
//
// Brushes are resolved once at construction time per CLAUDE.md line 91:
// SidebarPipelineProgress used to allocate four SolidColorBrush instances
// per LastProgress event (~10 Hz) until that hot-path lesson landed. The
// same rule applies here — a virtualized Library showing 200+ cards on a
// fast scroll would otherwise allocate brushes per visible chip per row.

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Theme.Controls;

public sealed partial class TagChip : UserControl
{
    // Property name is TagText (not Tag) because FrameworkElement already
    // defines a `Tag` object property of its own — naming our DP `Tag`
    // would hide the inherited one (CS0108) and force callers to fully
    // qualify it. `TagText` is unambiguous and names the type clearly.
    public static readonly DependencyProperty TagTextProperty =
        DependencyProperty.Register(
            nameof(TagText),
            typeof(string),
            typeof(TagChip),
            new PropertyMetadata(string.Empty, OnTagTextChanged));

    private static readonly SolidColorBrush ForegroundBrush =
        new(Color.FromArgb(0xCC, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush BackgroundBrush =
        new(Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF));

    public TagChip()
    {
        InitializeComponent();
        LabelText.Foreground = ForegroundBrush;
        ChipRoot.Background = BackgroundBrush;
    }

    public string TagText
    {
        get => (string)GetValue(TagTextProperty);
        set => SetValue(TagTextProperty, value);
    }

    private static void OnTagTextChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is TagChip c)
        {
            c.LabelText.Text = FormatTag((string?)e.NewValue ?? string.Empty);
        }
    }

    /// <summary>
    /// 1:1 port of macOS LibraryView.swift `formatTag(_:)`. Tags that
    /// already contain a space ("Has Faces") render as-is. Tags with
    /// underscores strip the hierarchical prefix and title-case the
    /// final segment ("animal_dog" → "Dog"). Dashes become spaces
    /// ("iPhone-14" → "Iphone 14"). Empty input returns empty.
    /// </summary>
    public static string FormatTag(string tag)
    {
        if (string.IsNullOrEmpty(tag)) return tag;
        if (tag.Contains(' ')) return tag;
        var segment = tag.Contains('_')
            ? tag[(tag.LastIndexOf('_') + 1)..]
            : tag;
        segment = segment.Replace('-', ' ');
        if (segment.Length == 0) return tag;
        return char.ToUpperInvariant(segment[0]) + segment[1..];
    }
}
