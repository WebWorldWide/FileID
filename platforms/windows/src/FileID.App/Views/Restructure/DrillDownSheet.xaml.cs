// DrillDownSheet code-behind. Filters the engine's RestructurePlan.Moves
// list down to a single source-folder + category pair (Sankey ribbon),
// or a single proposed folder (TreeDiff click), then renders one row
// per file with a shell-thumbnail.

using System;
using System.Collections.Generic;
using System.IO;
using FileID.IpcSchema;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.Restructure;

public sealed partial class DrillDownSheet : UserControl
{
    public DrillDownSheet()
    {
        InitializeComponent();
    }

    /// <summary>Filter to the moves whose source-folder bucket = `source` AND category = `category`.</summary>
    public void SetSankeyFilter(RestructurePlan plan, string source, string category)
    {
        HeaderText.Text = $"{source} → {category}";
        var moves = new List<RestructureMove>();
        var libRoot = plan.LibraryRoot ?? "";
        foreach (var m in plan.Moves)
        {
            var srcRel = TrimRoot(m.Source, libRoot);
            var srcBucket = TopLevel(srcRel);
            if (string.Equals(srcBucket, source, StringComparison.OrdinalIgnoreCase) &&
                string.Equals(m.Category, category, StringComparison.OrdinalIgnoreCase))
            {
                moves.Add(m);
            }
        }
        Render(moves);
    }

    /// <summary>Filter to the moves whose proposed destination starts with the given path.</summary>
    public void SetTreeFilter(RestructurePlan plan, string proposedPath)
    {
        HeaderText.Text = $"Proposed: {proposedPath}";
        var moves = new List<RestructureMove>();
        var libRoot = plan.LibraryRoot ?? "";
        foreach (var m in plan.Moves)
        {
            var dstRel = TrimRoot(m.Destination, libRoot);
            if (dstRel.StartsWith(proposedPath, StringComparison.OrdinalIgnoreCase))
            {
                moves.Add(m);
            }
        }
        Render(moves);
    }

    /// <summary>Filter to the moves whose engine Tier maps to the given outcome
    /// (Tidy = Mixed-tier, Reorganize = Junk-tier). Backs a recommendation card's
    /// "See all N files" — mirrors macOS drillDownSheet(.outcome(...)).</summary>
    internal void SetOutcomeFilter(RestructurePlan plan, FileID.ViewModels.RestructureOutcome outcome, string title)
    {
        HeaderText.Text = title;
        var moves = new List<RestructureMove>();
        foreach (var m in plan.Moves)
        {
            if (FileID.ViewModels.RestructureGrouping.OutcomeForTier(m.Tier) == outcome)
            {
                moves.Add(m);
            }
        }
        Render(moves);
    }

    private void Render(IList<RestructureMove> moves)
    {
        CountText.Text = $"{moves.Count} file{(moves.Count == 1 ? "" : "s")}";
        var rows = new List<UIElement>();
        foreach (var m in moves)
        {
            rows.Add(BuildRow(m));
        }
        FileRepeater.ItemsSource = rows;
    }

    private FrameworkElement BuildRow(RestructureMove m)
    {
        var grid = new Grid
        {
            Padding = new Thickness(10, 6, 10, 6),
            CornerRadius = new CornerRadius(8),
            Background = (Brush)Application.Current.Resources["SubtleFillColorSecondaryBrush"],
            ColumnSpacing = 12,
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var thumbHost = new Border
        {
            Width = 56,
            Height = 56,
            CornerRadius = new CornerRadius(6),
            Background = (Brush)Application.Current.Resources["SubtleFillColorTertiaryBrush"],
        };
        var img = new Image { Stretch = Stretch.UniformToFill, Width = 56, Height = 56 };
        thumbHost.Child = img;
        Grid.SetColumn(thumbHost, 0);
        grid.Children.Add(thumbHost);
        _ = LoadThumbAsync(img, m.Source);

        var labelStack = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        labelStack.Children.Add(new TextBlock
        {
            Text = Path.GetFileName(m.Source),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        // Plain-language "why filed here" (RESTRUCTURE.md §6 trust mechanic).
        if (!string.IsNullOrEmpty(m.Reason))
        {
            labelStack.Children.Add(new TextBlock
            {
                Text = m.Reason,
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
                Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
                TextWrapping = TextWrapping.Wrap,
            });
        }
        labelStack.Children.Add(new TextBlock
        {
            Text = $"{m.Source} → {m.Destination}",
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = (Brush)Application.Current.Resources["TextFillColorTertiaryBrush"],
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        Grid.SetColumn(labelStack, 1);
        grid.Children.Add(labelStack);

        // Right rail: butler confidence pill (auto / review / hold) above the
        // engine-stamped Anchor / Mixed / Junk tier pill.
        var badges = new StackPanel { Spacing = 4, VerticalAlignment = VerticalAlignment.Center };
        if (BuildConfidenceBadge(m.Confidence) is FrameworkElement cb) badges.Children.Add(cb);
        if (BuildTierBadge(m.Tier) is FrameworkElement tb) badges.Children.Add(tb);
        if (badges.Children.Count > 0)
        {
            Grid.SetColumn(badges, 2);
            grid.Children.Add(badges);
        }
        return grid;
    }

    /// <summary>Butler confidence pill — auto (green) / review (gold) / hold
    /// (amber). Null when the engine didn't stamp a confidence.</summary>
    private static FrameworkElement? BuildConfidenceBadge(string? confidence)
    {
        (string label, Windows.UI.Color accent) = confidence switch
        {
            "auto" => ("Auto-file", Windows.UI.Color.FromArgb(0xFF, 0x6C, 0xC2, 0x4A)),
            "review" => ("Review", Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00)),
            "ask" => ("Hold", Windows.UI.Color.FromArgb(0xFF, 0xF5, 0xB7, 0x4D)),
            _ => ("", default),
        };
        return string.IsNullOrEmpty(label) ? null : Pill(label, accent);
    }

    /// <summary>Anchor (gold) / Mixed (cyan) / Junk (pink) folder-tier pill.
    /// Null when the move has no tier (engine mismatch or skipped move).</summary>
    private static FrameworkElement? BuildTierBadge(string? tier)
    {
        Windows.UI.Color accent = tier switch
        {
            "Anchor" => Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00),
            "Mixed" => Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA),
            "Junk" => Windows.UI.Color.FromArgb(0xFF, 0xF2, 0xA6, 0xC0),
            _ => default,
        };
        return string.IsNullOrEmpty(tier) || accent.A == 0 ? null : Pill(tier!, accent);
    }

    /// <summary>A small rounded color-coded badge ("pill") used for both the
    /// confidence and folder-tier labels.</summary>
    private static Border Pill(string text, Windows.UI.Color accent)
    {
        var fill = accent; fill.A = 0x33;
        return new Border
        {
            Background = new SolidColorBrush(fill),
            BorderBrush = new SolidColorBrush(accent),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(8, 2, 8, 2),
            HorizontalAlignment = HorizontalAlignment.Right,
            Child = new TextBlock
            {
                Text = text,
                FontSize = 11,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                Foreground = new SolidColorBrush(accent),
            },
        };
    }

    private static async System.Threading.Tasks.Task LoadThumbAsync(Image img, string path)
    {
        // BitmapImage is a DispatcherObject. Capture img.DispatcherQueue
        // before any await; constructing the BitmapImage on the worker thread
        // that resumes the await is a known native fast-fail shape.
        var dispatcher = img.DispatcherQueue;
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            if (!File.Exists(path)) return;
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 128,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb != null && thumb.Size > 0 && dispatcher != null)
            {
                var captured = thumb;
                thumb = null;
                var enqueued = dispatcher.TryEnqueue(async () =>
                {
                    try
                    {
                        var bmp = new BitmapImage();
                        await bmp.SetSourceAsync(captured);
                        img.Source = bmp;
                    }
                    catch { /* swallow */ }
                    finally { try { captured.Dispose(); } catch { } }
                });
                if (!enqueued) { try { captured.Dispose(); } catch { } }
            }
        }
        catch { /* swallow */ }
        finally { try { thumb?.Dispose(); } catch { } }
    }

    private static string TrimRoot(string p, string root)
    {
        if (!string.IsNullOrEmpty(root) && p.StartsWith(root, StringComparison.OrdinalIgnoreCase))
        {
            p = p.Substring(root.Length);
        }
        return p.TrimStart('\\', '/');
    }

    private static string TopLevel(string rel)
    {
        var parts = rel.Split('\\', '/');
        return parts.Length > 1 ? parts[0] : "(root)";
    }
}
