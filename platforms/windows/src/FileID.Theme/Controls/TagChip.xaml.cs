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

    // Kind variant — slightly more opaque background than the AI chip so
    // the structured file-type label reads as primary metadata, foreground
    // at full white to differentiate from the gold-tinted Auto chip.
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
    /// 1:1 port of macOS <c>LibraryView.swift:646</c> <c>formatTag(_:)</c>.
    /// Tags that already contain a space ("Has Faces") render as-is. Tags
    /// with underscores strip the hierarchical prefix and uppercase the
    /// first character of the final segment ("animal_dog" → "Dog"). Dashes
    /// become spaces, and ONLY the leading character is uppercased — internal
    /// camelCase / model-number capitals are preserved
    /// ("iPhone-14" → "IPhone 14"). Empty input returns empty.
    /// </summary>
    public static string FormatTag(string tag)
    {
        if (string.IsNullOrEmpty(tag)) return tag;
        // Pre-formatted multi-word labels ("Has Faces", "Has TEXT") render
        // as-is — must come before the underscore / dash transforms so we
        // don't accidentally re-case an already-curated string.
        if (tag.Contains(' ')) return tag;

        // Take the last segment after '_' (hierarchical labels like
        // "animal_dog" become "dog"). Matches macOS split(separator: "_").last.
        var segment = tag.Contains('_')
            ? tag[(tag.LastIndexOf('_') + 1)..]
            : tag;

        // Dashes become spaces ("iPhone-14" → "iPhone 14").
        segment = segment.Replace('-', ' ');

        if (segment.Length == 0) return segment;
        // Uppercase only the first character, preserve everything else
        // exactly. Matches macOS `first.uppercased() + withSpaces.dropFirst()`.
        // We deliberately don't `ToTitleCase(ToLowerInvariant(...))` — that
        // would mangle "iPhone 14" → "Iphone 14" and lose model-number casing.
        return char.ToUpperInvariant(segment[0]) + segment[1..];
    }
}
