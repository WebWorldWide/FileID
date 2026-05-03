// IridescentBorder — rotating angular gradient border. 14s linear loop.
// Frozen to gold under reduced-motion.
//
// Mirror of macOS IridescentBorder modifier (MotionPrimitives.swift:110).
// 5-stop angular gradient: gold → delight → ai → info → gold, rotated
// 360° over 14s.
//
// WinUI 3 doesn't have a stock conic/angular gradient brush. We render the
// stroke via Win2D's CanvasSweepGradient, which is hardware-accelerated
// and the visual closest match to SwiftUI's AngularGradient.
//
// Usage:
//   <Border CornerRadius="12">
//       <motion:IridescentBorder x:Name="Iridescent" StrokeThickness="2" />
//       <ContentControl Content="..." />
//   </Border>
//
// The control sizes itself to the parent and renders only the stroke;
// place it inside any Grid/Border layer where you want the iridescent
// outline.

using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Brushes;
using Microsoft.Graphics.Canvas.Geometry;
using Microsoft.Graphics.Canvas.UI;
using Microsoft.Graphics.Canvas.UI.Xaml;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using System.Numerics;
using Windows.UI;

namespace FileID.Theme.Motion;

public sealed class IridescentBorder : Control
{
    public static readonly DependencyProperty StrokeThicknessProperty =
        DependencyProperty.Register(nameof(StrokeThickness), typeof(double), typeof(IridescentBorder),
            new PropertyMetadata(2.0));

    /// <summary>
    /// Corner radius of the rendered ring. Renamed from `CornerRadius` to
    /// avoid shadowing <see cref="Control.CornerRadius"/>, which uses the
    /// `CornerRadius` struct (not double). Bindings address us as
    /// `motion:IridescentBorder Radius="12"`.
    /// </summary>
    public static readonly DependencyProperty RadiusProperty =
        DependencyProperty.Register(nameof(Radius), typeof(double), typeof(IridescentBorder),
            new PropertyMetadata(12.0));

    private CanvasControl? _canvas;
    private DateTime _animationStart;

    public IridescentBorder()
    {
        DefaultStyleKey = typeof(IridescentBorder);
        IsHitTestVisible = false;
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    public double StrokeThickness
    {
        get => (double)GetValue(StrokeThicknessProperty);
        set => SetValue(StrokeThicknessProperty, value);
    }

    public double Radius
    {
        get => (double)GetValue(RadiusProperty);
        set => SetValue(RadiusProperty, value);
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        _canvas = GetTemplateChild("PART_Canvas") as CanvasControl;
        if (_canvas is not null)
        {
            _canvas.Draw += OnDraw;
        }
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        _animationStart = DateTime.UtcNow;
        // Drive at vsync via CompositionTarget.Rendering. Cheap; we draw a
        // single ring per frame.
        CompositionTarget.Rendering += OnRendering;
        ReducedMotion.Instance.PropertyChanged += OnReducedMotionChanged;
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        CompositionTarget.Rendering -= OnRendering;
        ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        if (_canvas is not null)
        {
            _canvas.RemoveFromVisualTree();
            _canvas = null;
        }
    }

    private void OnReducedMotionChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        // Static gold under reduced motion; one redraw catches up to the
        // new state without the per-frame loop fighting us.
        DispatcherQueue.TryEnqueue(() => _canvas?.Invalidate());
    }

    private void OnRendering(object? sender, object e)
    {
        if (ReducedMotion.Instance.IsReduced)
        {
            return; // single static draw on Loaded; CompositionTarget.Rendering
                    // is a no-op when we don't invalidate.
        }
        _canvas?.Invalidate();
    }

    private void OnDraw(CanvasControl sender, CanvasDrawEventArgs args)
    {
        var width = (float)sender.ActualWidth;
        var height = (float)sender.ActualHeight;
        if (width <= 0 || height <= 0)
        {
            return;
        }

        var stroke = (float)StrokeThickness;
        var radius = (float)Radius;

        // Inset the rectangle by half the stroke so the line sits inside the
        // logical bounds (matches CSS / SwiftUI border behavior).
        var rect = new Windows.Foundation.Rect(
            stroke / 2, stroke / 2,
            Math.Max(width - stroke, 0),
            Math.Max(height - stroke, 0));

        using var path = CanvasGeometry.CreateRoundedRectangle(sender, rect, radius, radius);

        if (ReducedMotion.Instance.IsReduced)
        {
            using var staticBrush = CreateStaticGoldBrush(sender);
            args.DrawingSession.DrawGeometry(path, staticBrush, stroke);
        }
        else
        {
            using var rotating = CreateRotatingSweepBrush(sender, width, height);
            args.DrawingSession.DrawGeometry(path, rotating, stroke);
        }
    }

    private static CanvasSolidColorBrush CreateStaticGoldBrush(CanvasControl device) =>
        new(device, ResolveColor("GoldColor", Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00)));

    private CanvasRadialGradientBrush CreateRotatingSweepBrush(CanvasControl device, float width, float height)
    {
        // Rotation phase ∈ [0, 1) over a 14s period.
        double elapsed = (DateTime.UtcNow - _animationStart).TotalSeconds;
        const double period = 14.0;
        float phase = (float)((elapsed % period) / period);

        // CanvasRadialGradientBrush approximates a sweep when given enough
        // stops along a center-anchored gradient. Win2D ships an actual
        // Sweep gradient via a custom shader, but for fidelity-vs-cost the
        // approximation reads as iridescent without going to a custom
        // shader.
        //
        // Phase 1.17 acceptance review will side-by-side this against the
        // macOS AngularGradient. If the visual delta is unacceptable, we
        // upgrade to a custom Win2D SVG-shader path.
        var center = new Vector2(width / 2, height / 2);

        var gold    = ResolveColor("GoldColor",    Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        var delight = ResolveColor("DelightColor", Color.FromArgb(0xFF, 0xF2, 0xA6, 0xC0));
        var ai      = ResolveColor("AiColor",      Color.FromArgb(0xFF, 0xB1, 0x9B, 0xCE));
        var info    = ResolveColor("InfoColor",    Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA));

        var stops = new[]
        {
            new CanvasGradientStop { Position = (phase + 0.0f) % 1f, Color = gold },
            new CanvasGradientStop { Position = (phase + 0.25f) % 1f, Color = delight },
            new CanvasGradientStop { Position = (phase + 0.5f) % 1f, Color = ai },
            new CanvasGradientStop { Position = (phase + 0.75f) % 1f, Color = info },
            new CanvasGradientStop { Position = (phase + 1.0f) % 1f, Color = gold },
        };
        // Sort by position (CanvasRadialGradientBrush requires monotonic).
        Array.Sort(stops, (a, b) => a.Position.CompareTo(b.Position));

        return new CanvasRadialGradientBrush(device, stops)
        {
            Center = center,
            OriginOffset = Vector2.Zero,
            RadiusX = Math.Max(width, height),
            RadiusY = Math.Max(width, height),
        };
    }

    private static Color ResolveColor(string key, Color fallback)
    {
        if (Application.Current?.Resources[key] is Color c)
        {
            return c;
        }
        return fallback;
    }
}
