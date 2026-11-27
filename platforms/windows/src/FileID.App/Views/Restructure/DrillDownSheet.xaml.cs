// DrillDownSheet code-behind. Filters the plan's moves down to one source-folder + category pair (Sankey ribbon) or one proposed folder (TreeDiff click) and renders a row per file.

using System;
using System.Collections.Generic;
using System.IO;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.Restructure;

public sealed partial class DrillDownSheet : UserControl
{
    private static readonly Windows.UI.Color GreenAccent = Windows.UI.Color.FromArgb(0xFF, 0x6C, 0xC2, 0x4A);
    private static readonly Windows.UI.Color GoldAccent = Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00);
    private static readonly Windows.UI.Color AmberAccent = Windows.UI.Color.FromArgb(0xFF, 0xF5, 0xB7, 0x4D);
    private static readonly Windows.UI.Color CyanAccent = Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA);
    private static readonly Windows.UI.Color PinkAccent = Windows.UI.Color.FromArgb(0xFF, 0xF2, 0xA6, 0xC0);

    private static readonly System.Collections.Generic.Dictionary<uint, (SolidColorBrush Fill, SolidColorBrush Accent)> PillBrushes =
        BuildPillBrushes();

    private static System.Collections.Generic.Dictionary<uint, (SolidColorBrush, SolidColorBrush)> BuildPillBrushes()
    {
        var map = new System.Collections.Generic.Dictionary<uint, (SolidColorBrush, SolidColorBrush)>();
        foreach (var accent in new[] { GreenAccent, GoldAccent, AmberAccent, CyanAccent, PinkAccent })
        {
            var fill = accent; fill.A = 0x33;
            map[Key(accent)] = (new SolidColorBrush(fill), new SolidColorBrush(accent));
        }
        return map;
    }

    private static uint Key(Windows.UI.Color c)
        => ((uint)c.A << 24) | ((uint)c.R << 16) | ((uint)c.G << 8) | c.B;

    // BuildRow creates a UIElement and a shell thumbnail request per move, and ItemsRepeater over UIElements does not virtualize; an unbounded list froze the UI thread. The rest is summarized in the count line.
    private const int MaxRenderedRows = 200;

    public DrillDownSheet()
    {
        InitializeComponent();
    }

    internal static IList<RestructureFileRowVm> PrepareSelectableRows(IList<RestructureFileRowVm> rows)
        => rows;

    internal void SetSelectableRows(IList<RestructureFileRowVm> rows, string title)
    {
        HeaderText.Text = title;
        CountText.Text = $"{rows.Count:N0} file{(rows.Count == 1 ? "" : "s")}";
        FileRepeater.Visibility = Visibility.Collapsed;
        SelectableRepeater.ItemsSource = PrepareSelectableRows(rows);
        SelectableRepeater.Visibility = Visibility.Visible;
    }

    private void OnSelectionElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement element) return;
        element.DataContext = RestructureView.ResolveRepeaterItem<RestructureFileRowVm>(
            sender.ItemsSource,
            args.Index);
    }

    private void OnSelectableCheckClicked(object sender, RoutedEventArgs e)
    {
        if (sender is CheckBox checkBox && checkBox.DataContext is RestructureFileRowVm row)
        {
            row.IsSelected = checkBox.IsChecked == true;
        }
    }

    public void SetSankeyFilter(RestructurePlan plan, string source, string category)
    {
        HeaderText.Text = $"{source} → {category}";
        var libRoot = plan.LibraryRoot ?? "";
        var all = plan.Moves;
        string SrcRaw(RestructureMove m) => TopLevel(TrimRoot(m.Source, libRoot));

        const int MaxNodes = 12;
        const string OtherKey = "Other";
        int distinctSources = all.Select(SrcRaw).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        var topSources = all.GroupBy(SrcRaw).OrderByDescending(g => g.Count())
            .Take(MaxNodes - 1).Select(g => g.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        string SrcBucket(RestructureMove m)
        {
            var s = SrcRaw(m);
            return distinctSources > MaxNodes && !topSources.Contains(s) ? OtherKey : s;
        }
        int distinctCats = all.Select(m => m.Category).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        var topCats = all.GroupBy(m => m.Category).OrderByDescending(g => g.Count())
            .Take(MaxNodes - 1).Select(g => g.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        string CatBucket(RestructureMove m)
        {
            var c = m.Category;
            return distinctCats > MaxNodes && !topCats.Contains(c) ? OtherKey : c;
        }

        var moves = new List<RestructureMove>();
        foreach (var m in all)
        {
            if (string.Equals(SrcBucket(m), source, StringComparison.OrdinalIgnoreCase) &&
                string.Equals(CatBucket(m), category, StringComparison.OrdinalIgnoreCase))
            {
                moves.Add(m);
            }
        }
        Render(moves);
    }

    public void SetTreeFilter(RestructurePlan plan, string proposedPath)
    {
        HeaderText.Text = $"Going to: {proposedPath}";
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
        int shown = Math.Min(moves.Count, MaxRenderedRows);
        CountText.Text = moves.Count > shown
            ? $"{moves.Count:N0} files - showing the first {shown:N0}"
            : $"{moves.Count} file{(moves.Count == 1 ? "" : "s")}";
        var rows = new List<UIElement>(shown);
        for (int i = 0; i < shown; i++)
        {
            rows.Add(BuildRow(moves[i]));
        }
        FileRepeater.ItemsSource = rows;
    }

    private FrameworkElement BuildRow(RestructureMove m)
    {
        var grid = new Grid
        {
            Padding = new Thickness(10, 6, 10, 6),
            CornerRadius = new CornerRadius(8),
            Background = FileID.Services.ThemeHelper.GetBrushSafe("SubtleFillColorSecondaryBrush"),
            ColumnSpacing = 12,
        };
        // A screen reader announces each row by its file name.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(grid, Path.GetFileName(m.Source));
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var thumbHost = new Border
        {
            Width = 56,
            Height = 56,
            CornerRadius = new CornerRadius(6),
            Background = FileID.Services.ThemeHelper.GetBrushSafe("SubtleFillColorTertiaryBrush"),
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
            Style = FileID.Services.ThemeHelper.GetStyleSafe("BodyStrongTextBlockStyle")!,
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        if (!string.IsNullOrEmpty(m.Reason))
        {
            labelStack.Children.Add(new TextBlock
            {
                Text = m.Reason,
                Style = FileID.Services.ThemeHelper.GetStyleSafe("CaptionTextBlockStyle")!,
                Foreground = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush"),
                TextWrapping = TextWrapping.Wrap,
            });
        }
        labelStack.Children.Add(new TextBlock
        {
            Text = $"{m.Source} → {m.Destination}",
            Style = FileID.Services.ThemeHelper.GetStyleSafe("CaptionTextBlockStyle")!,
            Foreground = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorTertiaryBrush"),
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        Grid.SetColumn(labelStack, 1);
        grid.Children.Add(labelStack);

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

    private static FrameworkElement? BuildConfidenceBadge(string? confidence)
    {
        (string label, Windows.UI.Color accent) = confidence switch
        {
            "auto" => ("Auto-file", GreenAccent),
            "review" => ("Review", GoldAccent),
            "ask" => ("Hold", AmberAccent),
            _ => ("", default),
        };
        return string.IsNullOrEmpty(label) ? null : Pill(label, accent);
    }

    private static FrameworkElement? BuildTierBadge(string? tier)
    {
        Windows.UI.Color accent = tier switch
        {
            "Anchor" => GoldAccent,
            "Mixed" => CyanAccent,
            "Junk" => PinkAccent,
            _ => default,
        };
        return string.IsNullOrEmpty(tier) || accent.A == 0 ? null : Pill(tier!, accent);
    }

    private static Border Pill(string text, Windows.UI.Color accent)
    {
        if (!PillBrushes.TryGetValue(Key(accent), out var brushes))
        {
            var fill = accent; fill.A = 0x33;
            brushes = (new SolidColorBrush(fill), new SolidColorBrush(accent));
        }
        return new Border
        {
            Background = brushes.Fill,
            BorderBrush = brushes.Accent,
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(8, 2, 8, 2),
            HorizontalAlignment = HorizontalAlignment.Right,
            Child = new TextBlock
            {
                Text = text,
                FontSize = 11,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                Foreground = brushes.Accent,
            },
        };
    }

    private static async System.Threading.Tasks.Task LoadThumbAsync(Image img, string path)
    {
        // Capture the dispatcher before any await: building a BitmapImage on the thread the await resumes on is a native fast-fail.
        var dispatcher = img.DispatcherQueue;
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            if (!File.Exists(path)) return;
            if (Services.ThumbnailService.SkipShellThumbnailForExtension(path)) return;
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
