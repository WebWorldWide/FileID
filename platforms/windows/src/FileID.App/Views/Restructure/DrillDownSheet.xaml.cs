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

        var thumbHost = new Border
        {
            Width = 56, Height = 56, CornerRadius = new CornerRadius(6),
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
        return grid;
    }

    private static async System.Threading.Tasks.Task LoadThumbAsync(Image img, string path)
    {
        try
        {
            if (!File.Exists(path)) return;
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            using var thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 128,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb != null && thumb.Size > 0)
            {
                var bmp = new BitmapImage();
                await bmp.SetSourceAsync(thumb);
                img.Source = bmp;
            }
        }
        catch { /* swallow */ }
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
