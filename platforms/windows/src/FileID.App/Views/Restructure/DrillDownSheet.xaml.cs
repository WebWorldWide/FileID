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
        labelStack.Children.Add(new TextBlock
        {
            Text = $"{m.Source} → {m.Destination}",
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = (Brush)Application.Current.Resources["TextFillColorTertiaryBrush"],
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        Grid.SetColumn(labelStack, 1);
        grid.Children.Add(labelStack);

        // engine-stamped tier badge (Anchor / Mixed / Junk).
        // Anchor = gold (#FFCC00), Mixed = cyan (#A0E2EA), Junk = pink (#F2A6C0).
        if (BuildTierBadge(m.Tier) is FrameworkElement badge)
        {
            Grid.SetColumn(badge, 2);
            grid.Children.Add(badge);
        }
        return grid;
    }

    /// <summary>render an Anchor/Mixed/Junk badge for a move,
    /// using the FileID palette colors. Returns null when the move
    /// has no tier (engine version mismatch or skipped move).</summary>
    private static FrameworkElement? BuildTierBadge(string? tier)
    {
        if (string.IsNullOrEmpty(tier)) return null;
        Windows.UI.Color colorAccent;
        switch (tier)
        {
            case "Anchor": colorAccent = Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00); break;
            case "Mixed": colorAccent = Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA); break;
            case "Junk": colorAccent = Windows.UI.Color.FromArgb(0xFF, 0xF2, 0xA6, 0xC0); break;
            default: return null;
        }
        var fill = colorAccent; fill.A = 0x33;
        var border = new Border
        {
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(fill),
            BorderBrush = new Microsoft.UI.Xaml.Media.SolidColorBrush(colorAccent),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(8, 2, 8, 2),
            VerticalAlignment = VerticalAlignment.Center,
        };
        border.Child = new TextBlock
        {
            Text = tier,
            FontSize = 11,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(colorAccent),
        };
        return border;
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
