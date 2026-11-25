// TagChip — the small tag chip Library cards show below the filename. The
// static `FormatTag` helper is applied by the control itself, so callers bind
// raw classifier output without a value converter.
//
// Brushes are resolved once at construction: a virtualized Library scrolling
// 200+ cards would otherwise allocate brushes per visible chip.

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Theme.Controls;

/// <summary>Visual variant for <see cref="TagChip"/>. `Auto` is the
/// gold-tinted AI-tag chip; `Kind` is the gray structured-metadata chip
/// used for the file-type label that leads each card's chip row.</summary>
public enum ChipVariant
{
    Auto = 0,
    Kind = 1,
}

public sealed partial class TagChip : UserControl
{
    // Named TagText because FrameworkElement already defines `Tag`; a DP named `Tag` would hide it (CS0108).
    public static readonly DependencyProperty TagTextProperty =
        DependencyProperty.Register(
            nameof(TagText),
            typeof(string),
            typeof(TagChip),
            new PropertyMetadata(string.Empty, OnTagTextChanged));

    public static readonly DependencyProperty VariantProperty =
        DependencyProperty.Register(
            nameof(Variant),
            typeof(ChipVariant),
            typeof(TagChip),
            new PropertyMetadata(ChipVariant.Auto, OnVariantChanged));

    private static readonly SolidColorBrush FallbackForeground =
        new(Color.FromArgb(0xCC, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush FallbackBackground =
        new(Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF));

    // Kind variant: a more opaque background and full-white foreground set it apart from the gold-tinted Auto chip.
    private static readonly SolidColorBrush FallbackKindForeground =
        new(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush FallbackKindBackground =
        new(Color.FromArgb(0x4D, 0x80, 0x80, 0x80));

    private static SolidColorBrush? _cachedFg;
    private static SolidColorBrush? _cachedBg;
    private static SolidColorBrush? _cachedKindFg;
    private static SolidColorBrush? _cachedKindBg;

    public TagChip()
    {
        InitializeComponent();
        _cachedFg ??= ResolveBrush("TagChipForegroundBrush", FallbackForeground);
        _cachedBg ??= ResolveBrush("TagChipBackgroundBrush", FallbackBackground);
        _cachedKindFg ??= ResolveBrush("TagChipKindForegroundBrush", FallbackKindForeground);
        _cachedKindBg ??= ResolveBrush("TagChipKindBackgroundBrush", FallbackKindBackground);
        ApplyVariant();
    }

    private static SolidColorBrush ResolveBrush(string key, SolidColorBrush fallback)
    {
        if (Application.Current?.Resources?.TryGetValue(key, out var obj) == true
            && obj is SolidColorBrush b)
        {
            return b;
        }
        return fallback;
    }

    public string TagText
    {
        get => (string)GetValue(TagTextProperty);
        set => SetValue(TagTextProperty, value);
    }

    public ChipVariant Variant
    {
        get => (ChipVariant)GetValue(VariantProperty);
        set => SetValue(VariantProperty, value);
    }

    private static void OnTagTextChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is TagChip c)
        {
            c.LabelText.Text = FormatTag((string?)e.NewValue ?? string.Empty);
        }
    }

    private static void OnVariantChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is TagChip c) c.ApplyVariant();
    }

    private void ApplyVariant()
    {
        if (Variant == ChipVariant.Kind)
        {
            LabelText.Foreground = _cachedKindFg;
            ChipRoot.Background = _cachedKindBg;
        }
        else
        {
            LabelText.Foreground = _cachedFg;
            ChipRoot.Background = _cachedBg;
        }
    }

    /// <summary>
    /// Formats a raw tag for display. Tags that already contain a space ("Has Faces") render as-is. Tags
    /// with underscores strip the hierarchical prefix and uppercase the
    /// first character of the final segment ("animal_dog" → "Dog"). Dashes
    /// become spaces, and ONLY the leading character is uppercased — internal
    /// camelCase / model-number capitals are preserved
    /// ("iPhone-14" → "IPhone 14"). Empty input returns empty.
    /// </summary>
    public static string FormatTag(string tag)
    {
        if (string.IsNullOrEmpty(tag)) return tag;
        // Pre-formatted labels ("Has Faces") render as-is, before the underscore/dash transforms.
        if (tag.Contains(' ')) return tag;

        // Hierarchical labels ("animal_dog") keep the last segment.
        var segment = tag.Contains('_')
            ? tag[(tag.LastIndexOf('_') + 1)..]
            : tag;

        segment = segment.Replace('-', ' ');

        if (segment.Length == 0) return segment;
        // Not ToTitleCase(ToLowerInvariant(...)): that would turn "iPhone 14" into "Iphone 14".
        return char.ToUpperInvariant(segment[0]) + segment[1..];
    }
}
