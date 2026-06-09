// SankeyFlowControl — pure-XAML Sankey diagram (no Win2D dep).
//
// Renders source-folder → category
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
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer? _renderDebounce;

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

    // Pre-cached brushes. Render() used to allocate ~2 brushes per ribbon
    // (idle/hover) plus 1 per category rect plus 2 more on every hover
    // (white highlight). With 12 sources × 12 categories that's up to
    // 288 SolidColorBrush allocations per Render call, and Render fires on
    // Loaded + SizeChanged — including during tab-swap mid-scan when the
    // dispatcher is under burst load from EngineClient. Per CLAUDE.md's
    // V15.2/V15.4 history, naked DispatcherObject construction on a
    // mid-transition XAML tree is a fast-fail shape. Cache once at ctor
    // (UI thread, dispatcher healthy) and reuse.
    private readonly Brush[] _ribbonIdleBrushes;
    private readonly Brush[] _ribbonHoverBrushes;
    private readonly Brush[] _categoryRectBrushes;
    private readonly SolidColorBrush _whiteHighlight;
    private readonly Brush _sourceBrush;

    // Last size we actually laid out at. SizeChanged fires on every pixel of a
    // resize drag; the layout is pixel-quantized, so only re-render when the
    // rounded size actually changed (debounce). -1 forces the first render.
    private int _lastRenderW = -1;
    private int _lastRenderH = -1;

    public SankeyFlowControl()
    {
        DefaultStyleKey = typeof(SankeyFlowControl);

        _sourceBrush = ResolveBrush("GoldBrush", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        // Okabe-Ito colour-blind-safe categorical palette (RESTRUCTURE.md §7):
        // distinct destination-category hues legible across all CVD types.
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
        // Debounce resize: a drag-resize fires SizeChanged many times per
        // second; coalesce into one Render ~80 ms after the last change.
        // RenderIfResized then skips renders where the pixel-quantized size
        // didn't actually change.
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

    /// <summary>Fires (source, category) when the user clicks a ribbon.</summary>
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

    // SizeChanged fires for every pixel during a resize drag. The layout is
    // quantized to integer pixels, so re-rendering only when the rounded size
    // actually changes keeps the O(S * C) layout off the hot drag path.
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
        // Record the size being evaluated UP FRONT — even when we bail sub-threshold
        // below — so the RenderIfResized debounce doesn't suppress a later restore to a
        // prior size after a sub-threshold pass blanked the canvas (which would leave
        // the diagram permanently blank, e.g. after the Flow/Tree toggle or a
        // narrow-then-widen). The cache must reflect the last size SEEN, not the last
        // size successfully DRAWN.
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

        // Cap each column at MaxNodes; fold the long tail into a single "Other"
        // node rather than silently dropping it (RESTRUCTURE.md §7).
        const int MaxNodes = 12;
        const string OtherKey = "Other";

        var distinctSources = moves.Select(SourceOf).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        var topSources = moves.GroupBy(SourceOf).OrderByDescending(g => g.Count())
            .Take(MaxNodes - 1).Select(g => g.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        string SourceBucket(RestructureMove m)
        {
            var s = SourceOf(m);
            return distinctSources > MaxNodes && !topSources.Contains(s) ? OtherKey : s;
        }

        var distinctCats = moves.Select(m => m.Category).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        var topCats = moves.GroupBy(m => m.Category).OrderByDescending(g => g.Count())
            .Take(MaxNodes - 1).Select(g => g.Key).ToHashSet(StringComparer.OrdinalIgnoreCase);
        string CategoryBucket(RestructureMove m)
        {
            var c = m.Category;
            return distinctCats > MaxNodes && !topCats.Contains(c) ? OtherKey : c;
        }

        var rawSourceGroups = moves.GroupBy(SourceBucket)
                                   .OrderByDescending(g => g.Count())
                                   .ToList();
        var rawCategoryGroups = moves.GroupBy(CategoryBucket)
                                     .OrderByDescending(g => g.Count())
                                     .ToList();

        var sourceList = rawSourceGroups.Select(g => g.Key).ToList();
        var categoryList = rawCategoryGroups.Select(g => g.Key).ToList();

        // Precompute the (source, category) flow matrix in a single pass over
        // moves. The barycentric sort below + the ribbon-draw loop used to call
        // moves.Count(predicate) for every (source, category) cell — O(2 * S * C
        // * Moves) per Render, re-firing on every SizeChanged during a resize
        // drag. One pass + dictionary lookups makes each cell O(1).
        var flow = new Dictionary<(string Src, string Cat), int>();
        foreach (var m in moves)
        {
            var key = (SourceBucket(m), CategoryBucket(m));
            flow.TryGetValue(key, out var n);
            flow[key] = n + 1;
        }
        int FlowOf(string src, string cat) => flow.TryGetValue((src, cat), out var n) ? n : 0;

        // 2 iterations of barycentric sorting to minimize ribbon crossings
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

        // Brushes are pre-allocated at ctor (see field declarations).
        // Aliases here keep the rest of Render() readable.
        var sourceBrush = _sourceBrush;

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

        // Draw the source rectangles + labels.
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

    // Nearest ribbon within the proximity threshold of the canvas-space point,
    // or null. Shared by hover (PointerMoved) and tap (OnTapped) so a touch/pen
    // tap — which never produces a hover phase — still resolves a ribbon.
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
        // Hit-test at the tap point rather than relying on _hovered: touch/pen
        // taps have no preceding hover phase, so _hovered would be null and the
        // tap was silently dropped.
        var hit = HitTest(e.GetPosition(_canvas));
        if (hit is { } h)
        {
            RibbonInvoked?.Invoke(this, (h.Source, h.Category));
        }
    }

    private void ApplyHover(Ribbon? hovered)
    {
        // Reset all to idle.
        foreach (var r in _ribbons) r.Path.Fill = r.IdleFill;
        foreach (var (rect, fill) in _rectIdleFill) rect.Fill = fill;
        if (_hoverTooltip != null) { _canvas?.Children.Remove(_hoverTooltip); _hoverTooltip = null; }
        if (hovered is null) return;

        // Highlight the hovered ribbon + its source/destination rects.
        // White-highlight brush is pre-allocated to avoid per-hover
        // DispatcherObject construction during pointer moves.
        hovered.Path.Fill = hovered.HoverFill;
        if (_sourceRects.TryGetValue(hovered.Source, out var srcRect))
        {
            srcRect.Fill = _whiteHighlight;
        }
        if (_categoryRects.TryGetValue(hovered.Category, out var catRect))
        {
            catRect.Fill = _whiteHighlight;
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
        => FileID.Services.ThemeHelper.GetBrushSafe(key, new SolidColorBrush(fallback));
}
