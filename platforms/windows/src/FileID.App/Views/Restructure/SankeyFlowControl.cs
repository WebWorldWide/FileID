// SankeyFlowControl: Sankey diagram built from plain XAML shapes.

using System;
using System.Collections.Generic;
using System.Linq;
using FileID.IpcSchema;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;

namespace FileID.Views.Restructure;

public sealed class SankeyFlowControl : Control
{
    private Canvas? _canvas;
    private RestructurePlan? _plan;
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer? _renderDebounce;

    private sealed class Ribbon
    {
        public required Microsoft.UI.Xaml.Shapes.Path Path { get; init; }
        public required string Source { get; init; }
        public required string Category { get; init; }
        public required int Count { get; init; }
        public required Brush IdleFill { get; init; }
        public required Brush HoverFill { get; init; }
        public required Point[] Samples { get; init; }
    }
    private readonly List<Ribbon> _ribbons = new();
    private readonly Dictionary<string, Rectangle> _sourceRects = new();
    private readonly Dictionary<string, Rectangle> _categoryRects = new();
    private readonly Dictionary<Rectangle, Brush> _rectIdleFill = new();
    private TextBlock? _hoverTooltip;

    // Up to 288 brushes (12 sources x 12 categories) would otherwise be allocated per Render, which fires on Loaded and SizeChanged, including under dispatcher load mid-scan.
    private readonly Brush[] _ribbonIdleBrushes;
    private readonly Brush[] _ribbonHoverBrushes;
    private readonly Brush[] _categoryRectBrushes;
    private readonly SolidColorBrush _whiteHighlight;
    private readonly Brush _sourceBrush;

    private int _lastRenderW = -1;
    private int _lastRenderH = -1;

    public SankeyFlowControl()
    {
        DefaultStyleKey = typeof(SankeyFlowControl);

        _sourceBrush = ResolveBrush("GoldBrush", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        // Brand gold/lavender/cyan/pink stay chrome-only (source rects, shell).
        var categoryColors = new[]
        {
            Color.FromArgb(0xFF, 0xE6, 0x9F, 0x00), // orange
            Color.FromArgb(0xFF, 0x56, 0xB4, 0xE9), // sky blue
            Color.FromArgb(0xFF, 0x00, 0x9E, 0x73), // bluish green
            Color.FromArgb(0xFF, 0xF0, 0xE4, 0x42), // yellow
            Color.FromArgb(0xFF, 0x00, 0x72, 0xB2), // blue
            Color.FromArgb(0xFF, 0xD5, 0x5E, 0x00), // vermillion
            Color.FromArgb(0xFF, 0xCC, 0x79, 0xA7), // reddish purple
        };
        _ribbonIdleBrushes = new Brush[categoryColors.Length];
        _ribbonHoverBrushes = new Brush[categoryColors.Length];
        _categoryRectBrushes = new Brush[categoryColors.Length];
        for (int i = 0; i < categoryColors.Length; i++)
        {
            var c = categoryColors[i];
            _ribbonIdleBrushes[i] = new SolidColorBrush(Color.FromArgb(0x66, c.R, c.G, c.B));
            _ribbonHoverBrushes[i] = new SolidColorBrush(Color.FromArgb(0xCC, c.R, c.G, c.B));
            _categoryRectBrushes[i] = new SolidColorBrush(c);
        }
        _whiteHighlight = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));

        Loaded += (_, _) => Render();
        _renderDebounce = DispatcherQueue?.CreateTimer();
        if (_renderDebounce is not null)
        {
            _renderDebounce.Interval = TimeSpan.FromMilliseconds(80);
            _renderDebounce.IsRepeating = false;
            _renderDebounce.Tick += (_, _) => RenderIfResized();
            SizeChanged += (_, _) => { _renderDebounce.Stop(); _renderDebounce.Start(); };
        }
        else
        {
            SizeChanged += OnSizeChanged;
        }
        PointerMoved += OnPointerMoved;
        PointerExited += OnPointerExited;
        Tapped += OnTapped;
    }

    public event EventHandler<(string Source, string Category)>? RibbonInvoked;

    public void SetPlan(RestructurePlan? plan)
    {
        _plan = plan;
        Render();
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        _canvas = GetTemplateChild("PART_Canvas") as Canvas;
        Render();
    }

    private void OnSizeChanged(object sender, SizeChangedEventArgs e) => RenderIfResized();

    // Layout is quantized to integer pixels, so re-rendering only when the rounded size changes keeps the O(S * C) layout off the drag path.
    private void RenderIfResized()
    {
        int w = (int)Math.Round(ActualWidth);
        int h = (int)Math.Round(ActualHeight);
        if (w == _lastRenderW && h == _lastRenderH) return;
        Render();
    }

    private void Render()
    {
        if (_canvas is null) return;
        // Record the size up front, even for a sub-threshold pass that blanks the canvas; otherwise the RenderIfResized debounce would suppress a later restore to the prior size and leave the diagram blank.
        _lastRenderW = (int)Math.Round(ActualWidth);
        _lastRenderH = (int)Math.Round(ActualHeight);
        _canvas.Children.Clear();
        _ribbons.Clear();
        _sourceRects.Clear();
        _categoryRects.Clear();
        _rectIdleFill.Clear();
        _hoverTooltip = null;
        if (_plan is null || _plan.Moves.Count == 0 || ActualWidth < 100 || ActualHeight < 60) return;

        var moves = _plan.Moves;

        string SourceOf(RestructureMove m)
        {
            var rel = m.Source;
            if (!string.IsNullOrEmpty(_plan.LibraryRoot) &&
                rel.StartsWith(_plan.LibraryRoot, StringComparison.OrdinalIgnoreCase))
            {
                rel = rel.Substring(_plan.LibraryRoot.Length).TrimStart('\\', '/');
            }
            var parts = rel.Split('\\', '/');
            return parts.Length > 1 ? parts[0] : "(root)";
        }

        const int MaxNodes = 12;
        const string OtherKey = "Other";

        int n = moves.Count;
        var srcOf = new string[n];
        var srcRawCounts = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        var catRawCounts = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        for (int i = 0; i < n; i++)
        {
            var s = SourceOf(moves[i]);
            srcOf[i] = s;
            srcRawCounts.TryGetValue(s, out var sc);
            srcRawCounts[s] = sc + 1;
            var cat = moves[i].Category;
            catRawCounts.TryGetValue(cat, out var cc);
            catRawCounts[cat] = cc + 1;
        }

        var topSources = srcRawCounts.OrderByDescending(kv => kv.Value)
            .Take(MaxNodes - 1).Select(kv => kv.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        bool foldSources = srcRawCounts.Count > MaxNodes;
        var topCats = catRawCounts.OrderByDescending(kv => kv.Value)
            .Take(MaxNodes - 1).Select(kv => kv.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        bool foldCats = catRawCounts.Count > MaxNodes;

        var srcBucketOf = new string[n];
        var catBucketOf = new string[n];
        var flow = new Dictionary<(string Src, string Cat), int>();
        for (int i = 0; i < n; i++)
        {
            var sb = foldSources && !topSources.Contains(srcOf[i]) ? OtherKey : srcOf[i];
            var cb = foldCats && !topCats.Contains(moves[i].Category) ? OtherKey : moves[i].Category;
            srcBucketOf[i] = sb;
            catBucketOf[i] = cb;
            flow.TryGetValue((sb, cb), out var fn);
            flow[(sb, cb)] = fn + 1;
        }
        int FlowOf(string src, string cat) => flow.TryGetValue((src, cat), out var f) ? f : 0;

        // Group over move indices with the precomputed bucket keys; only each group's Key and Count are used downstream.
        var rawSourceGroups = Enumerable.Range(0, n).GroupBy(i => srcBucketOf[i])
                                        .OrderByDescending(g => g.Count())
                                        .ToList();
        var rawCategoryGroups = Enumerable.Range(0, n).GroupBy(i => catBucketOf[i])
                                          .OrderByDescending(g => g.Count())
                                          .ToList();

        var sourceList = rawSourceGroups.Select(g => g.Key).ToList();
        var categoryList = rawCategoryGroups.Select(g => g.Key).ToList();

        for (int iter = 0; iter < 2; iter++)
        {
            var catWeights = new Dictionary<string, double>();
            foreach (var cat in categoryList)
            {
                double weightedSum = 0;
                double totalWeight = 0;
                for (int sIdx = 0; sIdx < sourceList.Count; sIdx++)
                {
                    var f = FlowOf(sourceList[sIdx], cat);
                    if (f > 0)
                    {
                        weightedSum += sIdx * f;
                        totalWeight += f;
                    }
                }
                catWeights[cat] = totalWeight > 0 ? (weightedSum / totalWeight) : 0.0;
            }
            categoryList = categoryList.OrderBy(c => catWeights[c]).ToList();

            var srcWeights = new Dictionary<string, double>();
            foreach (var src in sourceList)
            {
                double weightedSum = 0;
                double totalWeight = 0;
                for (int cIdx = 0; cIdx < categoryList.Count; cIdx++)
                {
                    var f = FlowOf(src, categoryList[cIdx]);
                    if (f > 0)
                    {
                        weightedSum += cIdx * f;
                        totalWeight += f;
                    }
                }
                srcWeights[src] = totalWeight > 0 ? (weightedSum / totalWeight) : 0.0;
            }
            sourceList = sourceList.OrderBy(s => srcWeights[s]).ToList();
        }

        var bySource = sourceList.Select(sKey => rawSourceGroups.First(g => g.Key == sKey)).ToList();
        var byCategory = categoryList.Select(cKey => rawCategoryGroups.First(g => g.Key == cKey)).ToList();
        if (bySource.Count == 0 || byCategory.Count == 0) return;

        const double margin = 24;
        const double labelGutter = 8;
        const double boxWidth = 14;
        var w = ActualWidth;
        var h = ActualHeight;
        var totalSrc = bySource.Sum(g => g.Count());
        var totalCat = byCategory.Sum(g => g.Count());
        var availH = h - 2 * margin;
        if (availH <= 0) return;

        var srcYs = new Dictionary<string, (double y, double height)>();
        double cursor = margin;
        const double gap = 6;
        var srcAvail = availH - gap * (bySource.Count - 1);
        foreach (var g in bySource)
        {
            var height = Math.Max(8, srcAvail * g.Count() / Math.Max(1, totalSrc));
            srcYs[g.Key] = (cursor, height);
            cursor += height + gap;
        }
        var catYs = new Dictionary<string, (double y, double height)>();
        cursor = margin;
        var catAvail = availH - gap * (byCategory.Count - 1);
        foreach (var g in byCategory)
        {
            var height = Math.Max(8, catAvail * g.Count() / Math.Max(1, totalCat));
            catYs[g.Key] = (cursor, height);
            cursor += height + gap;
        }

        var sourceBrush = _sourceBrush;

        var srcOffsets = bySource.ToDictionary(g => g.Key, _ => 0.0);
        var catOffsets = byCategory.ToDictionary(g => g.Key, _ => 0.0);

        for (int s = 0; s < bySource.Count; s++)
        {
            var src = bySource[s];
            for (int c = 0; c < byCategory.Count; c++)
            {
                var cat = byCategory[c];
                var pairCount = FlowOf(src.Key, cat.Key);
                if (pairCount == 0) continue;

                var srcPos = srcYs[src.Key];
                var catPos = catYs[cat.Key];
                var srcRibbonH = srcPos.height * pairCount / Math.Max(1, src.Count());
                var catRibbonH = catPos.height * pairCount / Math.Max(1, cat.Count());
                var srcStart = srcPos.y + srcOffsets[src.Key];
                var catStart = catPos.y + catOffsets[cat.Key];
                srcOffsets[src.Key] += srcRibbonH;
                catOffsets[cat.Key] += catRibbonH;

                var brushIdx = c % _ribbonIdleBrushes.Length;
                var idleFill = _ribbonIdleBrushes[brushIdx];
                var hoverFill = _ribbonHoverBrushes[brushIdx];
                var (path, samples) = AddRibbon(_canvas,
                    margin + boxWidth, srcStart, srcRibbonH,
                    w - margin - boxWidth, catStart, catRibbonH,
                    idleFill);
                _ribbons.Add(new Ribbon
                {
                    Path = path,
                    Source = src.Key,
                    Category = cat.Key,
                    Count = pairCount,
                    IdleFill = idleFill,
                    HoverFill = hoverFill,
                    Samples = samples,
                });
            }
        }

        foreach (var g in bySource)
        {
            var pos = srcYs[g.Key];
            var rect = new Rectangle
            {
                Width = boxWidth,
                Height = pos.height,
                Fill = sourceBrush,
                RadiusX = 3,
                RadiusY = 3,
            };
            Canvas.SetLeft(rect, margin);
            Canvas.SetTop(rect, pos.y);
            _canvas.Children.Add(rect);
            _sourceRects[g.Key] = rect;
            _rectIdleFill[rect] = sourceBrush;

            var label = NodeLabel(TrimLabel(g.Key, 22), g.Count(), g.Key);
            Canvas.SetLeft(label, margin + boxWidth + labelGutter);
            Canvas.SetTop(label, pos.y + Math.Max(0, pos.height / 2 - 8));
            _canvas.Children.Add(label);
        }

        for (int i = 0; i < byCategory.Count; i++)
        {
            var g = byCategory[i];
            var pos = catYs[g.Key];
            var brushIdx = i % _categoryRectBrushes.Length;
            var rect = new Rectangle
            {
                Width = boxWidth,
                Height = pos.height,
                Fill = _categoryRectBrushes[brushIdx],
                RadiusX = 3,
                RadiusY = 3,
            };
            Canvas.SetLeft(rect, w - margin - boxWidth);
            Canvas.SetTop(rect, pos.y);
            _canvas.Children.Add(rect);
            _categoryRects[g.Key] = rect;
            _rectIdleFill[rect] = rect.Fill;

            var label = NodeLabel(TrimLabelStart(g.Key, 28), g.Count(), g.Key);
            label.Measure(new Size(260, 20));
            Canvas.SetLeft(label, w - margin - boxWidth - labelGutter - label.DesiredSize.Width);
            Canvas.SetTop(label, pos.y + Math.Max(0, pos.height / 2 - 8));
            _canvas.Children.Add(label);
        }
    }

    private static (Microsoft.UI.Xaml.Shapes.Path path, Point[] samples) AddRibbon(Canvas canvas,
        double srcX, double srcY, double srcH,
        double dstX, double dstY, double dstH,
        Brush fill)
    {
        var midX = (srcX + dstX) / 2;
        var srcTop = new Point(srcX, srcY);
        var srcBot = new Point(srcX, srcY + srcH);
        var dstTop = new Point(dstX, dstY);
        var dstBot = new Point(dstX, dstY + dstH);

        var topCp1 = new Point(midX, srcY);
        var topCp2 = new Point(midX, dstY);
        var botCp1 = new Point(midX, dstY + dstH);
        var botCp2 = new Point(midX, srcY + srcH);

        var fig = new PathFigure { StartPoint = srcTop, IsClosed = true, IsFilled = true };
        fig.Segments.Add(new BezierSegment { Point1 = topCp1, Point2 = topCp2, Point3 = dstTop });
        fig.Segments.Add(new LineSegment { Point = dstBot });
        fig.Segments.Add(new BezierSegment { Point1 = botCp1, Point2 = botCp2, Point3 = srcBot });
        fig.Segments.Add(new LineSegment { Point = srcTop });

        var geom = new PathGeometry();
        geom.Figures.Add(fig);
        var p = new Microsoft.UI.Xaml.Shapes.Path
        {
            Data = geom,
            Fill = fill,
        };
        canvas.Children.Add(p);

        var midSrcY = srcY + srcH * 0.5;
        var midDstY = dstY + dstH * 0.5;
        var p0 = new Point(srcX, midSrcY);
        var p1 = new Point(midX, midSrcY);
        var p2 = new Point(midX, midDstY);
        var p3 = new Point(dstX, midDstY);
        const int N = 24;
        var samples = new Point[N + 1];
        for (int i = 0; i <= N; i++)
        {
            double t = (double)i / N;
            samples[i] = CubicBezier(p0, p1, p2, p3, t);
        }
        return (p, samples);
    }

    private static Point CubicBezier(Point p0, Point p1, Point p2, Point p3, double t)
    {
        double u = 1 - t;
        double b0 = u * u * u;
        double b1 = 3 * u * u * t;
        double b2 = 3 * u * t * t;
        double b3 = t * t * t;
        return new Point(
            b0 * p0.X + b1 * p1.X + b2 * p2.X + b3 * p3.X,
            b0 * p0.Y + b1 * p1.Y + b2 * p2.Y + b3 * p3.Y);
    }

    // Shared by hover and tap so a touch or pen tap, which has no hover phase, still resolves a ribbon.
    private Ribbon? HitTest(Point pos)
    {
        const double HitThreshold = 14.0;
        Ribbon? best = null;
        double bestDist = HitThreshold;
        foreach (var r in _ribbons)
        {
            foreach (var s in r.Samples)
            {
                var dx = s.X - pos.X;
                var dy = s.Y - pos.Y;
                var d = Math.Sqrt(dx * dx + dy * dy);
                if (d < bestDist) { bestDist = d; best = r; }
            }
        }
        return best;
    }

    private void OnPointerMoved(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_canvas is null || _ribbons.Count == 0) return;
        ApplyHover(HitTest(e.GetCurrentPoint(_canvas).Position));
    }

    private void OnPointerExited(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        ApplyHover(null);
    }

    private void OnTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (_canvas is null || _ribbons.Count == 0) return;
        var hit = HitTest(e.GetPosition(_canvas));
        if (hit is { } h)
        {
            RibbonInvoked?.Invoke(this, (h.Source, h.Category));
        }
    }

    private void ApplyHover(Ribbon? hovered)
    {
        foreach (var r in _ribbons) r.Path.Fill = r.IdleFill;
        foreach (var (rect, fill) in _rectIdleFill) rect.Fill = fill;
        if (_hoverTooltip != null) { _canvas?.Children.Remove(_hoverTooltip); _hoverTooltip = null; }
        if (hovered is null) return;

        // The highlight brush is pre-allocated to avoid DispatcherObject construction on every pointer move.
        hovered.Path.Fill = hovered.HoverFill;
        if (_sourceRects.TryGetValue(hovered.Source, out var srcRect))
        {
            srcRect.Fill = _whiteHighlight;
        }
        if (_categoryRects.TryGetValue(hovered.Category, out var catRect))
        {
            catRect.Fill = _whiteHighlight;
        }
        var anchor = hovered.Samples[hovered.Samples.Length / 2];
        _hoverTooltip = new TextBlock
        {
            Text = $"{TrimLabel(hovered.Source, 22)} → {hovered.Category}  ({hovered.Count})",
            FontSize = 11,
            Foreground = ResolveBrush("TextFillColorPrimaryBrush", Colors.White),
            Padding = new Thickness(6, 3, 6, 3),
        };
        Canvas.SetLeft(_hoverTooltip, Math.Max(0, anchor.X - 60));
        Canvas.SetTop(_hoverTooltip, Math.Max(0, anchor.Y - 22));
        _canvas?.Children.Add(_hoverTooltip);
    }

    private static string TrimLabel(string s, int max)
        => s.Length <= max ? s : string.Concat(s.AsSpan(0, max - 1), "…");

    internal static string TrimLabelStart(string s, int max)
        => s.Length <= max ? s : string.Concat("…", s.AsSpan(s.Length - (max - 1)));

    private static StackPanel NodeLabel(string name, int count, string fullPath)
    {
        var label = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        label.Children.Add(new TextBlock
        {
            Text = name,
            FontSize = 12,
            Foreground = ResolveBrush("Text1Brush", Colors.White),
        });
        label.Children.Add(new TextBlock
        {
            Text = count.ToString("N0"),
            FontSize = 11.5,
            Foreground = ResolveBrush("Text3Brush", Color.FromArgb(0xFF, 0xA6, 0xAC, 0xB3)),
            VerticalAlignment = VerticalAlignment.Center,
        });
        ToolTipService.SetToolTip(label, fullPath);
        return label;
    }

    private static Brush ResolveBrush(string key, Color fallback)
        => FileID.Services.ThemeHelper.GetBrushSafe(key, new SolidColorBrush(fallback));
}
