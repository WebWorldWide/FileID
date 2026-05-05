// SankeyFlowControl — pure-XAML Sankey diagram (no Win2D dep).
//
// Mirror of macOS SankeyFlowView. Renders source-folder → category
// flows with cubic-bezier ribbons whose thickness is proportional to
// the file count flowing through them. Bezier paths use
// Microsoft.UI.Xaml.Shapes.Path with PathFigure + BezierSegment so
// every render is GPU-composited at vsync.
//
// Inputs: a collection of (source, category) pairs from the engine's
// RestructurePlan.Moves list.

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

    // Per-rendered ribbon: keep enough state to do proximity hit-testing
    // + cross-highlight on pointer move. Rebuilt on each Render().
    private sealed class Ribbon
    {
        public required Microsoft.UI.Xaml.Shapes.Path Path { get; init; }
        public required string Source { get; init; }
        public required string Category { get; init; }
        public required int Count { get; init; }
        public required Brush IdleFill { get; init; }
        public required Brush HoverFill { get; init; }
        // Sample points along the bezier mid-line for cheap proximity test.
        public required Point[] Samples { get; init; }
    }
    private readonly List<Ribbon> _ribbons = new();
    private readonly Dictionary<string, Rectangle> _sourceRects = new();
    private readonly Dictionary<string, Rectangle> _categoryRects = new();
    private readonly Dictionary<Rectangle, Brush> _rectIdleFill = new();
    private TextBlock? _hoverTooltip;

    public SankeyFlowControl()
    {
        DefaultStyleKey = typeof(SankeyFlowControl);
        Loaded += (_, _) => Render();
        SizeChanged += (_, _) => Render();
        PointerMoved += OnPointerMoved;
        PointerExited += OnPointerExited;
        Tapped += OnTapped;
    }

    /// <summary>Fires (source, category) when the user clicks a ribbon.</summary>
    public event EventHandler<(string Source, string Category)>? RibbonInvoked;

    private Ribbon? _hovered;

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

    private void Render()
    {
        if (_canvas is null) return;
        _canvas.Children.Clear();
        _ribbons.Clear();
        _sourceRects.Clear();
        _categoryRects.Clear();
        _rectIdleFill.Clear();
        _hoverTooltip = null;
        if (_plan is null || _plan.Moves.Count == 0 || ActualWidth < 100 || ActualHeight < 60) return;

        var moves = _plan.Moves;

        // Group: source = top-level subfolder of the library root (or "(root)").
        // Sink   = category from the engine's classifier.
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

        var bySource = moves.GroupBy(SourceOf)
                            .OrderByDescending(g => g.Count())
                            .Take(12)
                            .ToList();
        var byCategory = moves.GroupBy(m => m.Category)
                              .OrderByDescending(g => g.Count())
                              .Take(12)
                              .ToList();
        if (bySource.Count == 0 || byCategory.Count == 0) return;

        // Layout: left column of source rects, right column of category rects.
        // Box height ∝ file count; ribbon thickness ∝ pair count.
        const double margin = 24;
        const double labelGutter = 8;
        const double boxWidth = 14;
        var w = ActualWidth;
        var h = ActualHeight;
        var totalSrc = bySource.Sum(g => g.Count());
        var totalCat = byCategory.Sum(g => g.Count());
        var availH = h - 2 * margin;
        if (availH <= 0) return;

        // Compute per-source y/height + per-category y/height (left-aligned vertical stacks).
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

        // Brushes: gold for sources, lavender/cyan/pink rotation for categories.
        var sourceBrush = ResolveBrush("GoldBrush", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        var categoryColors = new[]
        {
            ResolveColor("AiBrush",      Color.FromArgb(0xFF, 0xB1, 0x9B, 0xCE)),
            ResolveColor("InfoBrush",    Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA)),
            ResolveColor("DelightBrush", Color.FromArgb(0xFF, 0xF2, 0xA6, 0xC0)),
        };

        // Draw ribbons first (so labels + boxes overlay them).
        // Each pair (source, category) → one bezier path.
        // Within each source/category we keep a running offset so multiple
        // ribbons don't overlap on the box edge.
        var srcOffsets = bySource.ToDictionary(g => g.Key, _ => 0.0);
        var catOffsets = byCategory.ToDictionary(g => g.Key, _ => 0.0);

        for (int s = 0; s < bySource.Count; s++)
        {
            var src = bySource[s];
            for (int c = 0; c < byCategory.Count; c++)
            {
                var cat = byCategory[c];
                var pairCount = src.Count(m => m.Category == cat.Key);
                if (pairCount == 0) continue;

                var srcPos = srcYs[src.Key];
                var catPos = catYs[cat.Key];
                var srcRibbonH = srcPos.height * pairCount / Math.Max(1, src.Count());
                var catRibbonH = catPos.height * pairCount / Math.Max(1, cat.Count());
                var srcStart = srcPos.y + srcOffsets[src.Key];
                var catStart = catPos.y + catOffsets[cat.Key];
                srcOffsets[src.Key] += srcRibbonH;
                catOffsets[cat.Key] += catRibbonH;

                var ribbonColor = categoryColors[c % categoryColors.Length];
                var idleFill = new SolidColorBrush(Color.FromArgb(0x66, ribbonColor.R, ribbonColor.G, ribbonColor.B));
                var hoverFill = new SolidColorBrush(Color.FromArgb(0xCC, ribbonColor.R, ribbonColor.G, ribbonColor.B));
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

        // Draw the source rectangles + labels.
        foreach (var g in bySource)
        {
            var pos = srcYs[g.Key];
            var rect = new Rectangle
            {
                Width = boxWidth,
                Height = pos.height,
                Fill = sourceBrush,
                RadiusX = 3, RadiusY = 3,
            };
            Canvas.SetLeft(rect, margin);
            Canvas.SetTop(rect, pos.y);
            _canvas.Children.Add(rect);
            _sourceRects[g.Key] = rect;
            _rectIdleFill[rect] = sourceBrush;

            var label = new TextBlock
            {
                Text = $"{TrimLabel(g.Key, 22)}  ({g.Count()})",
                FontSize = 12,
                Foreground = ResolveBrush("TextFillColorPrimaryBrush", Colors.White),
            };
            Canvas.SetLeft(label, margin + boxWidth + labelGutter);
            Canvas.SetTop(label, pos.y + Math.Max(0, pos.height / 2 - 8));
            _canvas.Children.Add(label);
        }

        // Draw the category rectangles + labels.
        for (int i = 0; i < byCategory.Count; i++)
        {
            var g = byCategory[i];
            var pos = catYs[g.Key];
            var color = categoryColors[i % categoryColors.Length];
            var rect = new Rectangle
            {
                Width = boxWidth,
                Height = pos.height,
                Fill = new SolidColorBrush(color),
                RadiusX = 3, RadiusY = 3,
            };
            Canvas.SetLeft(rect, w - margin - boxWidth);
            Canvas.SetTop(rect, pos.y);
            _canvas.Children.Add(rect);
            _categoryRects[g.Key] = rect;
            _rectIdleFill[rect] = rect.Fill;

            var label = new TextBlock
            {
                Text = $"{g.Key}  ({g.Count()})",
                FontSize = 12,
                Foreground = ResolveBrush("TextFillColorPrimaryBrush", Colors.White),
                TextAlignment = TextAlignment.Right,
            };
            label.Measure(new Size(220, 20));
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

        // Sample the ribbon's CENTERLINE bezier at 24 points for the
        // proximity hit-test. The centerline is the average of the
        // top + bottom bezier control sets — visually equivalent to the
        // ribbon's mid-curve. Done once at render time so PointerMoved is
        // O(ribbons * 24) per move event (cheap).
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

    private void OnPointerMoved(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_canvas is null || _ribbons.Count == 0) return;
        var pos = e.GetCurrentPoint(_canvas).Position;
        const double HoverThreshold = 14.0;
        Ribbon? best = null;
        double bestDist = HoverThreshold;
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
        ApplyHover(best);
    }

    private void OnPointerExited(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        ApplyHover(null);
    }

    private void OnTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (_hovered is { } h)
        {
            RibbonInvoked?.Invoke(this, (h.Source, h.Category));
        }
    }

    private void ApplyHover(Ribbon? hovered)
    {
        _hovered = hovered;
        // Reset all to idle.
        foreach (var r in _ribbons) r.Path.Fill = r.IdleFill;
        foreach (var (rect, fill) in _rectIdleFill) rect.Fill = fill;
        if (_hoverTooltip != null) { _canvas?.Children.Remove(_hoverTooltip); _hoverTooltip = null; }
        if (hovered is null) return;

        // Highlight the hovered ribbon + its source/destination rects.
        hovered.Path.Fill = hovered.HoverFill;
        if (_sourceRects.TryGetValue(hovered.Source, out var srcRect))
        {
            srcRect.Fill = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));
        }
        if (_categoryRects.TryGetValue(hovered.Category, out var catRect))
        {
            catRect.Fill = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));
        }
        // Tooltip — first sample point as anchor.
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

    private static Brush ResolveBrush(string key, Color fallback)
    {
        if (Application.Current?.Resources[key] is Brush b) return b;
        return new SolidColorBrush(fallback);
    }

    private static Color ResolveColor(string key, Color fallback)
    {
        if (Application.Current?.Resources[key] is SolidColorBrush b) return b.Color;
        if (Application.Current?.Resources[key] is Color c) return c;
        return fallback;
    }
}
